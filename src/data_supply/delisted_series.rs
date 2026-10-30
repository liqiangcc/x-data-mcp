//! `derived:delisted_series_check` — the M3 "a delisted series must not
//! disappear" internal item (M3 PR-5, `#446` §4.5/§10.1, AC-M3-6).
//!
//! What it protects. A delisted code keeps its terminal daily series forever:
//! the bars up to and including the delisting day are history, and the daily
//! run must never rewrite or drop them. A source that rewrites that history is
//! a `republish_data` decision (an explicit, audited revision), not something a
//! routine `update_data` may absorb silently. This item is the check that says
//! so: it writes **no** business table, only its own result (item evidence) and
//! the list of entities that failed. It never repairs, rewrites or deletes a
//! series — a failure stops at `partially_failed` and names the entities.
//!
//! The two facts it compares are frozen apart in time:
//!
//! - the **registration** (`Baseline`), resolved in the worker *before* any
//!   entity item of this run commits: `(record_count, first_day, last_day,
//!   content_digest, last_event_seq, delisting_day)` per terminal entity, taken
//!   from the 不复权 daily series. That is "上一代登记" — the state the previous
//!   generation left behind. It travels in the item's `frozen_inputs`, so a
//!   resumed run re-checks against the same numbers (§4.5 "不静默重建").
//! - the **current** state, read when the item runs (after the entity items).
//!
//! Allowed transition. The one legitimate change is the tail completion: the
//! run that first owns the delisting day's bars appends them, so `last_day`
//! advances to at most `delisting_day`, `record_count` grows, `first_day` is
//! unchanged and every event since the registration is an `append` (the commit
//! path already proved the new bars extend the stored prefix). Anything else —
//! a vanished series, a shortened series, a changed digest over the same span,
//! a `replace`/rewrite event, or a bar *after* the delisting day — is a
//! failure with the entity named.
//!
//! Ambiguity is excluded instead of guessed: an entity whose `security_master`
//! row carries `issuer_resolution='ambiguous'` (a code reused by two issuers)
//! has no decidable terminal state, so it is not registered at all (it is
//! reported in the resolution evidence, not silently dropped from the count).
//!
//! Layer position: this module is application logic. It reads through the
//! storage adapters (`security_master`, `market_series`, `market_events`) and
//! writes nothing; the backend methods in [`crate::sqlite_market::backend`] run
//! each phase inside one read snapshot.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::data_supply::generation::SOURCE_GENERATION_KEY;
use crate::market_data::{Adjustment, Period};
use crate::market_storage::Error;
use crate::security_master::{Exchange as MasterExchange, IssuerResolution};
use crate::sqlite_market::{security, series};
use crate::task_control::{self, ItemSpec};

/// `task_items.progress_unit` of this internal item: one dataset, not one
/// security (same unit as every other derived dataset item).
pub const PROGRESS_UNIT: &str = "derived_dataset";

/// Frozen-input key holding the [`Baseline`] this run checks against.
pub const BASELINE_KEY: &str = "delisted_series_baseline";

/// Frozen-input key of the entity count that resolved *without* being
/// registered (ambiguous reuse or an undecidable terminal state). Evidence
/// only: a non-empty list explains why `checked` can be smaller than the number
/// of delisted entities the main table knows.
pub const SKIPPED_KEY: &str = "delisted_series_skipped";

fn integrity(message: impl ToString) -> Error {
    Error::Integrity(message.to_string())
}

/// One terminal entity's registration: the facts the next generation must
/// still see, plus the delisting day that bounds a legitimate tail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registration {
    pub entity: String,
    /// Stored bar count of the 不复权 daily series.
    pub record_count: i64,
    /// First stored bar (epoch day).
    pub first_day: i64,
    /// Last stored bar (epoch day).
    pub last_day: i64,
    /// `market_series.version_id` = hex of the series content digest.
    pub content_digest: String,
    /// `market_series.last_event_seq` at registration time.
    pub last_event_seq: i64,
    /// Delisting day (epoch day); the tail may not pass it.
    pub delisting_day: i64,
}

/// Per-entity registrations captured before the run's entity items.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Baseline {
    /// Acceptance date (Shanghai) the registrations were taken on.
    pub as_of: NaiveDate,
    /// Sorted by entity.
    pub registrations: Vec<Registration>,
}

/// What the check found. Serialized verbatim as the item's evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Report {
    /// Registrations this run checked.
    pub registered: usize,
    /// Registrations whose facts are byte-for-byte the registered ones.
    pub unchanged: usize,
    /// Legitimate tail completions (bars appended up to the delisting day).
    pub tail_extended: Vec<TailExtension>,
    /// Failures, sorted by entity: the list the task surface reports.
    pub failures: Vec<Failure>,
}

/// One allowed tail completion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TailExtension {
    pub entity: String,
    pub registered_last_day: i64,
    pub observed_last_day: i64,
    pub observed_record_count: i64,
}

/// One failed entity with the registered state and what was observed instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Failure {
    pub entity: String,
    /// One of the reason constants below.
    pub reason: &'static str,
    pub registered: Registration,
    /// `None` when the series itself is gone.
    pub observed: Option<Observed>,
}

/// The current series facts of one checked entity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Observed {
    pub record_count: i64,
    pub first_day: i64,
    pub last_day: i64,
    pub content_digest: String,
    pub last_event_seq: i64,
}

impl Report {
    pub fn failed(&self) -> bool {
        !self.failures.is_empty()
    }

    /// Compact task-surface message naming the failing entities. The full
    /// detail (registered/observed values) stays in the item evidence.
    pub fn failure_message(&self) -> String {
        const NAMED: usize = 20;
        let named = self
            .failures
            .iter()
            .take(NAMED)
            .map(|failure| format!("{}({})", failure.entity, failure.reason))
            .collect::<Vec<_>>()
            .join(",");
        let more = self.failures.len().saturating_sub(NAMED);
        let suffix = if more > 0 {
            format!(" (+{more} more)")
        } else {
            String::new()
        };
        format!(
            "delisted series check failed for {} entit{}: {named}{suffix}",
            self.failures.len(),
            if self.failures.len() == 1 { "y" } else { "ies" }
        )
    }
}

/// A series is gone. Nothing is rebuilt (§4.5): the entity is listed and the
/// run ends `partially_failed`.
pub const MISSING_SERIES: &str = "missing_series";
/// The first stored bar moved: history was rewritten at the start.
pub const FIRST_DAY_CHANGED: &str = "first_day_changed";
/// The series lost bars (`record_count` or `last_day` went backwards).
pub const HISTORY_SHRUNK: &str = "history_shrunk";
/// Same span, different rows: history was rewritten in place.
pub const CONTENT_DIGEST_CHANGED: &str = "content_digest_changed";
/// The series grew past the delisting day: a delisted code cannot trade again.
pub const LAST_DAY_BEYOND_DELISTING: &str = "last_day_beyond_delisting";
/// Facts changed with no appended event: not a tail, so not allowed.
pub const TRANSITION_UNRECORDED: &str = "transition_unrecorded";
/// The change came from a `replace`/rewrite event, not an append.
pub const HISTORY_REWRITTEN: &str = "history_rewritten";

/// What the main table says about one entity's terminal state, accumulated
/// over every lifecycle interval of the code.
#[derive(Debug, Clone, Copy, Default)]
struct Terminal {
    /// Earliest known delisting day (epoch day) of any interval.
    delisting_day: Option<i64>,
    /// Any interval was attributed ambiguously (the code was reused).
    ambiguous: bool,
}

/// Terminal state of every code in the main table, keyed by entity.
type TerminalMap = BTreeMap<String, Terminal>;
/// Stored 不复权 daily series facts, keyed by entity.
type DailySummaries = BTreeMap<String, series::RawDailySummary>;
/// Kinds of the events recorded since a registration, oldest first.
type EventKinds = Result<Vec<String>, Error>;

impl Terminal {
    /// `Some(day)` when the entity is terminal as of `today_day`.
    fn delisted_by(&self, today_day: i64) -> Option<i64> {
        self.delisting_day.filter(|day| *day <= today_day)
    }
}

/// Entities that are terminal in the main table (delisting day known and not in
/// the future) but were **not** registered, with the reason. Evidence only, so
/// a shrinking `checked` count is explainable instead of silent.
fn skipped(terminal: &TerminalMap, summaries: &DailySummaries, today_day: i64) -> Vec<Value> {
    let mut rows = Vec::new();
    for (entity, state) in terminal {
        if state.delisted_by(today_day).is_none() {
            continue;
        }
        if state.ambiguous {
            rows.push(json!({"entity": entity, "reason": "issuer_ambiguous"}));
        } else if !summaries.contains_key(entity) {
            rows.push(json!({"entity": entity, "reason": "no_series"}));
        }
    }
    rows
}

/// One run's registration resolution: what to check and why anything terminal
/// was left out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub baseline: Baseline,
    /// Evidence only (see [`SKIPPED_KEY`]).
    pub skipped: Vec<Value>,
}

/// Registration set of one run: the terminal entities of the main table that
/// actually have a 不复权 daily series.
///
/// "Terminal" is `delisting_date <= today` on the main table (§4.5 item 2).
/// The design's alternative arm — `last_day` before the update window — is not
/// a separate predicate here: an entity that is not delisted and whose series
/// stopped is a suspension/missing-data question, and its own items already
/// report it. Guessing "terminal" from a stalled series would flag active
/// members, which §4.5.1 explicitly forbids ("不能把活跃股错当退市股").
pub fn resolve(c: &Connection, today: NaiveDate) -> Result<Resolution, Error> {
    let today_day = crate::compact_market::epoch_day(today);
    let mut terminal = TerminalMap::new();
    for row in security::read_security_master(c)? {
        if row.instrument_class != 0 {
            continue;
        }
        if !matches!(row.exchange, MasterExchange::Sh | MasterExchange::Sz) {
            continue;
        }
        let entry = terminal.entry(row.entity()).or_default();
        entry.ambiguous |= row.issuer_resolution == IssuerResolution::Ambiguous;
        if let Some(day) = row.delisting_date {
            let day = crate::compact_market::epoch_day(day);
            entry.delisting_day = Some(entry.delisting_day.map_or(day, |current| current.min(day)));
        }
    }
    let summaries = series::raw_daily_summaries(c)?;
    let mut registrations = Vec::new();
    for (entity, state) in &terminal {
        let (Some(day), false) = (state.delisted_by(today_day), state.ambiguous) else {
            continue;
        };
        let Some(summary) = summaries.get(entity) else {
            continue;
        };
        registrations.push(Registration {
            entity: entity.clone(),
            record_count: summary.record_count,
            first_day: summary.first_day,
            last_day: summary.last_day,
            content_digest: summary.version_id.clone(),
            last_event_seq: summary.last_event_seq,
            delisting_day: day,
        });
    }
    Ok(Resolution {
        skipped: skipped(&terminal, &summaries, today_day),
        baseline: Baseline {
            as_of: today,
            registrations,
        },
    })
}

/// What one registration's comparison concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Unchanged,
    TailExtended,
    Failed(&'static str),
}

/// Decide one entity from its registered facts, its current facts and (only
/// when a tail is the remaining explanation) the kinds of the events recorded
/// since the registration. Pure: the caller supplies the read results, so every
/// branch is testable without a database.
fn classify(
    registration: &Registration,
    observed: &Observed,
    events_after: impl FnOnce() -> EventKinds,
) -> Result<Verdict, Error> {
    // The registered state itself can be impossible: a delisted code cannot
    // own a bar after its delisting day. That is a finding even when nothing
    // changed since the registration, so it is checked before "unchanged".
    if registration.last_day > registration.delisting_day {
        return Ok(Verdict::Failed(LAST_DAY_BEYOND_DELISTING));
    }
    if observed.first_day == registration.first_day
        && observed.last_day == registration.last_day
        && observed.record_count == registration.record_count
        && observed.content_digest == registration.content_digest
    {
        return Ok(Verdict::Unchanged);
    }
    if observed.first_day != registration.first_day {
        return Ok(Verdict::Failed(FIRST_DAY_CHANGED));
    }
    if observed.last_day < registration.last_day
        || observed.record_count < registration.record_count
    {
        return Ok(Verdict::Failed(HISTORY_SHRUNK));
    }
    if observed.last_day > registration.delisting_day {
        return Ok(Verdict::Failed(LAST_DAY_BEYOND_DELISTING));
    }
    if observed.last_day == registration.last_day {
        // Same span, different rows: an in-place rewrite of history.
        return Ok(Verdict::Failed(CONTENT_DIGEST_CHANGED));
    }
    // A tail is the only remaining explanation. It is allowed only when the
    // commit path recorded it as an append: the append path proved the stored
    // bars are a prefix of the new ones, which a `replace` did not.
    let kinds = events_after()?;
    if kinds.is_empty() {
        return Ok(Verdict::Failed(TRANSITION_UNRECORDED));
    }
    if kinds.iter().all(|kind| is_append(kind)) {
        return Ok(Verdict::TailExtended);
    }
    Ok(Verdict::Failed(HISTORY_REWRITTEN))
}

fn is_append(kind: &str) -> bool {
    kind == "append"
}

/// Compare the registered facts with the current series facts. Read-only: the
/// item never repairs anything (§4.5 "不静默重建、不静默删除").
pub fn check(c: &Connection, baseline: &Baseline) -> Result<Report, Error> {
    let mut unchanged = 0usize;
    let mut tail_extended = Vec::new();
    let mut failures = Vec::new();
    for registration in &baseline.registrations {
        let summary = series::raw_daily_summary(c, &registration.entity)?.map(|summary| Observed {
            record_count: summary.record_count,
            first_day: summary.first_day,
            last_day: summary.last_day,
            content_digest: summary.version_id,
            last_event_seq: summary.last_event_seq,
        });
        let Some(observed) = summary else {
            failures.push(failure(registration, MISSING_SERIES, None));
            continue;
        };
        let verdict = classify(registration, &observed, || {
            Ok(series::events_after(
                c,
                &registration.entity,
                Period::Day,
                Adjustment::Raw,
                registration.last_event_seq,
            )?
            .into_iter()
            .map(|event| event.kind)
            .collect())
        })?;
        match verdict {
            Verdict::Unchanged => unchanged += 1,
            Verdict::TailExtended => tail_extended.push(TailExtension {
                entity: registration.entity.clone(),
                registered_last_day: registration.last_day,
                observed_last_day: observed.last_day,
                observed_record_count: observed.record_count,
            }),
            Verdict::Failed(reason) => failures.push(failure(registration, reason, Some(observed))),
        }
    }
    failures.sort_by(|a, b| a.entity.cmp(&b.entity));
    tail_extended.sort_by(|a, b| a.entity.cmp(&b.entity));
    Ok(Report {
        registered: baseline.registrations.len(),
        unchanged,
        tail_extended,
        failures,
    })
}

fn failure(
    registration: &Registration,
    reason: &'static str,
    observed: Option<Observed>,
) -> Failure {
    Failure {
        entity: registration.entity.clone(),
        reason,
        registered: registration.clone(),
        observed,
    }
}

/// Decode the baseline carried in an item's frozen inputs. A malformed or
/// missing baseline is corruption of the task's frozen work, never "nothing to
/// check": the item fails closed instead of reporting a clean run.
pub fn decode_baseline(value: &Value) -> Result<Baseline, Error> {
    serde_json::from_value(value.clone())
        .map_err(|error| integrity(format!("delisted series baseline unreadable: {error}")))
}

/// The derived item spec for one plan-run task. `generation` is the same
/// generation binding every other derived item of the run carries.
pub fn item_spec(generation: &Value, baseline: &Baseline, skipped: &[Value]) -> ItemSpec {
    let item_id = task_control::derived::DELISTED_SERIES_CHECK.to_string();
    let frozen = json!({
        SOURCE_GENERATION_KEY: generation.clone(),
        BASELINE_KEY: baseline,
        SKIPPED_KEY: skipped,
    });
    ItemSpec {
        target: task_control::derived::target(&item_id),
        progress_unit: PROGRESS_UNIT.into(),
        frozen_inputs: frozen,
        scope_keys: std::collections::BTreeSet::from([task_control::derived::delisted_scope()]),
        item_id,
    }
}

#[cfg(test)]
mod tests;
