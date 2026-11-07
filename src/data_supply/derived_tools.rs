//! Derived read-only tools (D-82, issue-433 PR-B): `get_tradability` and
//! `get_market_cap`.
//!
//! Both tools answer one row per requested entity and call the research read
//! models (`research::tradability`, `research::price_limit`,
//! `research::market_cap`) over one shared [`ResearchContext`] inside the
//! caller's `market.read` transaction. They do not go through the v2 `Query`
//! path: a derived answer has no dataset of its own, so it has no registered
//! version and no `snapshot_id` — the answer is reproducible from the input
//! dataset versions in `inputs` plus the algorithm labels in
//! `algorithm_versions` (Q7, design §2.3 path 1).
//!
//! Rules this module keeps:
//!
//! * Every requested entity gets exactly one row, including codes that are not
//!   in the main table and instruments that are not A shares. Nothing is
//!   dropped silently ("0 rows is not evidence").
//! * `unknown` is never rendered as tradable, and a missing input dataset is
//!   `unknown` + `input_absent`, not "no event" (G8).
//! * Prices are decimal strings rendered at the series' own `price_scale`; the
//!   tool never assumes two decimal places.
//! * `known_at` is refused with an explicit message: none of the inputs carries
//!   a usable A-share knowledge time (design §4).
//! * Algorithm labels come from the research constants, never from a literal
//!   copied into this file.
use chrono::NaiveDate;
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Value, json};

use super::wire::Security;
use crate::market_storage::Error;
use crate::research::context::ResearchContext;
use crate::research::market_cap::{MarketCapOptions, ShareScope, market_cap};
use crate::research::price_limit::{PriceLimitReport, PriceLimitState, price_limit_in};
use crate::research::temporal::Temporal;
use crate::research::tradability::{Tradability, TradabilityReport, tradability_in};
use crate::sqlite_market::derived;

/// Heap one `get_tradability` call reserves before it decodes anything, in
/// bytes.
///
/// The shared [`ResearchContext`] decodes the whole-table inputs once for the
/// batch: `suspension_intervals`, `security_master`, `trading_days`,
/// `risk_warning_intervals` and the raw-series spans — about 90k rows on the
/// 2026-11-01 production read (design §5.2). Measured with
/// `data_supply::mcp::tests::probe_derived_tools_peak_heap_and_latency`
/// (`cargo nextest run --run-ignored only -E 'test(probe_derived_tools)'`)
/// on a fixture built through the production writers at that scale — 7,400
/// main-table rows, 72,000 suspension rows, a 300-day calendar, the admitted
/// rule book:
///
/// * one call (1 entity, cold): **8.9 MiB** peak RSS-anon growth;
/// * further calls on the same fixture: 0.0–0.1 MiB;
/// * 1 / 10 / 100 entities: 207–266 / 159–204 / 258–312 ms.
///
/// The fixture's calendar is 300 days where production has 8,798, and it has no
/// risk-warning rows where production has about 1,300; at ~40 B and ~150 B per
/// row those add under 1 MiB, so a production call peaks near 10 MiB. The
/// reserve is 16 MiB: about 1.6x the measured peak, which absorbs a larger
/// `suspension_intervals` without letting one call take a quarter of the
/// service's memory. A call that would exceed it is refused with
/// `memory_budget_exceeded` before it allocates anything.
pub const READ_RESERVE: usize = 16 * 1024 * 1024;

/// `derived_dataset_versions` identity of the facts the tools read.
const MASTER: (&str, &str) = (
    crate::security_master::assembly::DATASET_KIND,
    crate::security_master::assembly::DATASET_KEY,
);
const SUSPENSIONS: (&str, &str) = (
    crate::security_master::suspensions::DATASET_KIND,
    crate::security_master::suspensions::DATASET_KEY,
);
const RISK_WARNINGS: (&str, &str) = (
    crate::security_master::risk_warnings::DATASET_KIND,
    crate::security_master::risk_warnings::DATASET_KEY,
);
const CALENDAR: (&str, &str) = (
    crate::trading_calendar::DATASET_KIND,
    crate::trading_calendar::DATASET_KEY,
);
const RULES: (&str, &str) = (
    crate::trading_rules::DATASET_KIND,
    crate::trading_rules::DATASET_KEY,
);

/// A research-layer failure keeps `integrity_error` distinct from storage
/// failures (issue-433 PR-B note 4); everything else is a storage-level error.
fn research(e: crate::research::Error) -> Error {
    match e {
        crate::research::Error::Integrity(message) => Error::Integrity(message),
        other => Error::Storage(other.to_string()),
    }
}

/// Render `units` at `scale` as a decimal string. `scale` is the series'
/// property, not a constant: two places for A shares, but the code does not
/// assume it.
fn price(units: i64, scale: u32) -> String {
    let divisor = 10_i64.pow(scale);
    let sign = if units < 0 { "-" } else { "" };
    let magnitude = units.unsigned_abs();
    format!(
        "{sign}{}.{:0width$}",
        magnitude / divisor.unsigned_abs(),
        magnitude % divisor.unsigned_abs(),
        width = scale as usize
    )
}

/// Render 分 (1/100 元) as a two-place decimal string. Separate from [`price`]
/// because the value is `i128`: `price × shares` overflows `i64` for large
/// issuers in principle, and a silently truncated market cap would be worse
/// than a wider type.
fn money(units: i128) -> String {
    let sign = if units < 0 { "-" } else { "" };
    let magnitude = units.unsigned_abs();
    format!("{sign}{}.{:02}", magnitude / 100, magnitude % 100)
}

/// Whether this database has `table` at all.
///
/// A legacy (pre-M2) database has neither the derived-dataset registry nor the
/// derived input tables, so nothing is registered and nothing can be proven:
/// every derived answer there is `unknown` + `input_absent`. Asking the
/// storage decoder instead would fail with "no such table", which is a
/// statement about the schema rather than about the securities; the
/// fail-closed reading of "no registration, no input" is the honest one.
fn has_table(c: &Connection, table: &str) -> Result<bool, Error> {
    let found: Option<i64> = c
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
            [table],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| Error::Storage(error.to_string()))?;
    Ok(found.is_some())
}

fn has_registry(c: &Connection) -> Result<bool, Error> {
    has_table(c, "derived_dataset_versions")
}

/// The registered version of one input dataset, `None` when it was never
/// written (or the database has no registry at all).
fn registered(c: &Connection, (kind, key): (&str, &str)) -> Result<Option<String>, Error> {
    if !has_registry(c)? {
        return Ok(None);
    }
    Ok(derived::read_version(c, kind, key)?.map(|row| row.version_id))
}

/// The registered version of each input dataset this answer read, in a stable
/// order. Unregistered inputs are left out: `temporal.datasets_without_known_at`
/// and the per-row `unknown` reasons already say what could not be read.
fn inputs(c: &Connection) -> Result<Vec<Value>, Error> {
    let mut out = Vec::new();
    for dataset in [MASTER, SUSPENSIONS, RISK_WARNINGS, CALENDAR, RULES] {
        if let Some(version_id) = registered(c, dataset)? {
            out.push(json!({"dataset": dataset.0, "version_id": version_id}));
        }
    }
    Ok(out)
}

fn state_of(tradability: Tradability) -> &'static str {
    match tradability {
        Tradability::Tradable => "tradable",
        Tradability::NotTradable(_) => "not_tradable",
        Tradability::Unknown(_) => "unknown",
    }
}

/// `get_tradability`: whether each A share could trade on `as_of`, plus that
/// day's price limits. One shared context decodes the whole-table inputs once
/// for the whole batch.
pub fn tradability(
    c: &Connection,
    securities: &[Security],
    as_of: NaiveDate,
) -> Result<Value, Error> {
    let ctx = ResearchContext::new(c);
    let temporal = Temporal {
        as_of,
        known_at: None,
    };

    // G8: a suspensions dataset that was never written is not "no suspension".
    // Without it, a day with a bar would answer `tradable` on missing evidence.
    if registered(c, SUSPENSIONS)?.is_none() {
        return Ok(absent_tradability(securities, as_of));
    }

    let mut columns = Columns::default();
    let mut without_known_at: Vec<&'static str> = Vec::new();

    for entity in securities {
        let symbol = entity.symbol();
        let report = tradability_in(&ctx, &symbol, temporal).map_err(research)?;
        let limit = price_limit_in(&ctx, &symbol, temporal).map_err(research)?;
        columns.push(&report, &limit);
        for dataset in report.temporal.datasets_without_known_at() {
            if !without_known_at.contains(&dataset) {
                without_known_at.push(dataset);
            }
        }
        for dataset in limit.temporal.datasets_without_known_at() {
            if !without_known_at.contains(&dataset) {
                without_known_at.push(dataset);
            }
        }
    }

    Ok(json!({
        "tool": "get_tradability",
        "layout": "table",
        "snapshot_time": chrono::Utc::now().to_rfc3339(),
        "as_of": as_of.to_string(),
        "entities": securities.iter().map(Security::symbol).collect::<Vec<_>>(),
        "columns": columns.into_value(),
        "inputs": inputs(c)?,
        "algorithm_versions": {
            "tradability": crate::research::tradability::ALGORITHM_VERSION,
            "price_limit": crate::research::price_limit::READ_ALGORITHM_VERSION,
        },
        "limitations": [
            "tradable means a 不复权 bar exists and no confirmed suspension covers the day; \
             it is not a claim that an order would have filled (one-price limit boards, \
             zero-volume bars)",
            "confirmed suspensions exist for SH from 2008 and not for SZ; a missing bar \
             without an announcement is unknown + data_gap, never suspended",
            "no known_at: the inputs carry no usable A-share knowledge time; \
             use get_security_state for facts that do",
        ],
        "temporal": {
            "as_of": as_of.to_string(),
            "known_at": Value::Null,
            "datasets_without_known_at": without_known_at,
        },
    }))
}

/// The all-`unknown` answer for a batch when the suspensions dataset has never
/// been written (G8). One row per entity, same columns, reason `input_absent`.
fn absent_tradability(securities: &[Security], as_of: NaiveDate) -> Value {
    let count = securities.len();
    json!({
        "tool": "get_tradability",
        "layout": "table",
        "snapshot_time": chrono::Utc::now().to_rfc3339(),
        "as_of": as_of.to_string(),
        "entities": securities.iter().map(Security::symbol).collect::<Vec<_>>(),
        "columns": {
            "state": vec!["unknown"; count],
            "reason": vec!["input_absent"; count],
            "suspension": vec![Value::Null; count],
            "flags": vec![Value::Null; count],
            "limit_state": vec!["unknown"; count],
            "limit_reason": vec!["input_absent"; count],
            "limit_candidates": vec![Value::Null; count],
            "risk_status": vec!["unknown"; count],
            "limit_up": vec![Value::Null; count],
            "limit_down": vec![Value::Null; count],
            "prev_close": vec![Value::Null; count],
            "prev_close_day": vec![Value::Null; count],
            "rule_id": vec![Value::Null; count],
        },
        "inputs": [],
        "algorithm_versions": {
            "tradability": crate::research::tradability::ALGORITHM_VERSION,
            "price_limit": crate::research::price_limit::READ_ALGORITHM_VERSION,
        },
        "limitations": [
            "the suspensions dataset has no registered version, so nothing can be \
             proven about any day; every row is unknown + input_absent",
        ],
        "temporal": {
            "as_of": as_of.to_string(),
            "known_at": Value::Null,
            "datasets_without_known_at": [],
        },
    })
}

/// The all-`unknown` market-cap answer for a batch whose input table is
/// missing: one row per entity, reason `input_absent`, and the same columns.
fn absent_market_cap(
    securities: &[Security],
    as_of: NaiveDate,
    scope: ShareScope,
    missing: &str,
) -> Value {
    let count = securities.len();
    json!({
        "tool": "get_market_cap",
        "layout": "table",
        "snapshot_time": chrono::Utc::now().to_rfc3339(),
        "as_of": as_of.to_string(),
        "scope": scope.as_str(),
        "entities": securities.iter().map(Security::symbol).collect::<Vec<_>>(),
        "columns": {
            "state": vec!["unknown"; count],
            "reason": vec!["input_absent"; count],
            "market_cap": vec![Value::Null; count],
            "close": vec![Value::Null; count],
            "shares": vec![Value::Null; count],
            "share_event_day": vec![Value::Null; count],
            "share_event_category": vec![Value::Null; count],
            "chain_break_day": vec![Value::Null; count],
            "reconciliation": vec!["not_available"; count],
        },
        "inputs": [],
        "algorithm_versions": {
            "market_cap": crate::research::market_cap::ALGORITHM_VERSION,
        },
        "limitations": [format!(
            "this database has no `{missing}` table, so no share count or price can be \
             proven; every row is unknown + input_absent"
        )],
        "temporal": {
            "as_of": as_of.to_string(),
            "known_at": Value::Null,
            "datasets_without_known_at": [],
        },
    })
}

/// Column-major accumulator for [`tradability`].
#[derive(Default)]
struct Columns {
    state: Vec<Value>,
    reason: Vec<Value>,
    suspension: Vec<Value>,
    flags: Vec<Value>,
    limit_state: Vec<Value>,
    limit_reason: Vec<Value>,
    limit_candidates: Vec<Value>,
    risk_status: Vec<Value>,
    limit_up: Vec<Value>,
    limit_down: Vec<Value>,
    prev_close: Vec<Value>,
    prev_close_day: Vec<Value>,
    rule_id: Vec<Value>,
}

impl Columns {
    fn push(&mut self, report: &TradabilityReport, limit: &PriceLimitReport) {
        self.state.push(json!(state_of(report.state)));
        self.reason.push(json!(report.reason_code()));
        self.suspension.push(match &report.suspension {
            Some(suspension) => json!(format!(
                "{} {}..{}",
                suspension.certainty.as_str(),
                suspension.start_day,
                suspension.end_day
            )),
            None => Value::Null,
        });
        // Reserved for future evidence flags; the design's example keeps it null.
        self.flags.push(Value::Null);
        self.limit_state.push(json!(limit.state.as_str()));
        self.limit_reason.push(match limit.state {
            // `known`/`unbounded` are answers, not reasons.
            PriceLimitState::Known | PriceLimitState::Unbounded => Value::Null,
            PriceLimitState::Unknown => json!(limit.reason.map(|r| r.as_str())),
        });
        self.limit_candidates.push(if limit.candidates.is_empty() {
            Value::Null
        } else {
            let text: Vec<String> = limit
                .candidates
                .iter()
                .map(|candidate| match candidate.band_bp {
                    Some(bp) => format!("{bp}bp"),
                    None => "unbounded".to_owned(),
                })
                .collect();
            json!(text.join("|"))
        });
        self.risk_status.push(match limit.status_class {
            Some(class) => json!(class.as_str()),
            None => json!("unknown"),
        });
        let scale = limit.price_scale;
        self.limit_up.push(match (limit.limit_up_units, scale) {
            (Some(units), Some(scale)) => json!(price(units, scale)),
            _ => Value::Null,
        });
        self.limit_down.push(match (limit.limit_down_units, scale) {
            (Some(units), Some(scale)) => json!(price(units, scale)),
            _ => Value::Null,
        });
        self.prev_close.push(match (limit.prev_close_units, scale) {
            (Some(units), Some(scale)) => json!(price(units, scale)),
            _ => Value::Null,
        });
        self.prev_close_day.push(
            limit
                .prev_close_day
                .map_or(Value::Null, |day| json!(day.to_string())),
        );
        self.rule_id
            .push(limit.rule_id.as_ref().map_or(Value::Null, |id| json!(id)));
    }

    fn into_value(self) -> Value {
        json!({
            "state": self.state,
            "reason": self.reason,
            "suspension": self.suspension,
            "flags": self.flags,
            "limit_state": self.limit_state,
            "limit_reason": self.limit_reason,
            "limit_candidates": self.limit_candidates,
            "risk_status": self.risk_status,
            "limit_up": self.limit_up,
            "limit_down": self.limit_down,
            "prev_close": self.prev_close,
            "prev_close_day": self.prev_close_day,
            "rule_id": self.rule_id,
        })
    }
}

/// `get_market_cap`: the market cap of each security on `as_of` at the
/// requested `scope`. `a_share_total` is always `unknown` (no admitted source
/// for historical A-share totals) and `reconciliation` is always
/// `not_available` (no current-share reference is stored); neither is hidden
/// behind an option.
pub fn market_cap_table(
    c: &Connection,
    securities: &[Security],
    as_of: NaiveDate,
    scope: ShareScope,
) -> Result<Value, Error> {
    // The same fail-closed gate as `get_tradability`: without the share-event
    // table (or the bar series it prices against) nothing can be proven about
    // any entity, and an empty answer must not read as "no market cap".
    for table in ["capital_events", "market_series"] {
        if !has_table(c, table)? {
            return Ok(absent_market_cap(securities, as_of, scope, table));
        }
    }

    let temporal = Temporal {
        as_of,
        known_at: None,
    };
    let options = MarketCapOptions::default();

    let mut state = Vec::new();
    let mut reason = Vec::new();
    let mut cap = Vec::new();
    let mut close = Vec::new();
    let mut shares = Vec::new();
    let mut event_day = Vec::new();
    let mut event_category = Vec::new();
    let mut chain_break_day = Vec::new();
    let mut reconciliation = Vec::new();
    let mut without_known_at: Vec<&'static str> = Vec::new();

    for entity in securities {
        let answer =
            market_cap(c, &entity.symbol(), temporal, scope, &options).map_err(research)?;
        state.push(json!(match answer.state {
            crate::research::market_cap::MarketCapState::Known => "known",
            crate::research::market_cap::MarketCapState::Unknown => "unknown",
        }));
        reason.push(match answer.reason {
            Some(reason) => json!(reason.as_str()),
            None => Value::Null,
        });
        // `value_units` is 分 × shares (元 × 100), so 元 is two decimal places.
        cap.push(match answer.value_units {
            Some(units) => json!(money(units)),
            None => Value::Null,
        });
        close.push(match (answer.price_cents, answer.price_scale) {
            (Some(units), Some(scale)) => json!(price(units, scale)),
            _ => Value::Null,
        });
        shares.push(answer.shares.map_or(Value::Null, |value| json!(value)));
        event_day.push(
            answer
                .share_day
                .map_or(Value::Null, |day| json!(day.to_string())),
        );
        event_category.push(
            answer
                .share_category
                .map_or(Value::Null, |category| json!(category)),
        );
        chain_break_day.push(
            answer
                .chain_break_day
                .map_or(Value::Null, |day| json!(day.to_string())),
        );
        // D-50 G2 needs a current-share reference that the main table does not
        // store (market_cap.rs header, audit T6), so the gate never ran.
        reconciliation.push(json!(
            crate::research::market_cap::Reconciliation::NotAvailable.as_str()
        ));
        for dataset in answer.temporal.datasets_without_known_at() {
            if !without_known_at.contains(&dataset) {
                without_known_at.push(dataset);
            }
        }
    }

    let mut limitations = vec![
        "reconciliation is always not_available: no current-share reference is stored, \
         so the end-of-history check never ran",
        "a_share_total is always unknown: no admitted source for historical A-share totals",
    ];
    if scope == ShareScope::CompanyTotal {
        limitations.push("company_total is A+B+H total shares, not A-share-only market cap");
    }

    Ok(json!({
        "tool": "get_market_cap",
        "layout": "table",
        "snapshot_time": chrono::Utc::now().to_rfc3339(),
        "as_of": as_of.to_string(),
        "scope": scope.as_str(),
        "entities": securities.iter().map(Security::symbol).collect::<Vec<_>>(),
        "columns": {
            "state": state,
            "reason": reason,
            "market_cap": cap,
            "close": close,
            "shares": shares,
            "share_event_day": event_day,
            "share_event_category": event_category,
            "chain_break_day": chain_break_day,
            "reconciliation": reconciliation,
        },
        "inputs": inputs_for_market_cap(c)?,
        "algorithm_versions": {
            "market_cap": crate::research::market_cap::ALGORITHM_VERSION,
        },
        "limitations": limitations,
        "temporal": {
            "as_of": as_of.to_string(),
            "known_at": Value::Null,
            "datasets_without_known_at": without_known_at,
        },
    }))
}

/// The inputs `get_market_cap` reads. Capital events are registered per entity,
/// not once per dataset, so they are named in the response but not versioned
/// (design §2.2: use `list_data_versions` for those).
fn inputs_for_market_cap(c: &Connection) -> Result<Vec<Value>, Error> {
    let mut out = Vec::new();
    if let Some(version_id) = registered(c, MASTER)? {
        out.push(json!({"dataset": MASTER.0, "version_id": version_id}));
    }
    Ok(out)
}
