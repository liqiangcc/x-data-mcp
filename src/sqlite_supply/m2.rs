//! M2 parsed-fact datasets on the v8 read surface (issue-391 §12.3).
//!
//! Layer position (separation points 4/5): this module is the entry adapter's
//! storage branch. It reads the stored facts through `sqlite_market`'s typed
//! accessors, applies the `as_of`/`known_at` semantics in the order §12.2
//! fixes (information set first, record set second), and returns `Row::Fact`
//! cells — the columnar projection stays in `data_supply::columnar`, the
//! semantics stay in `research::temporal`.
//!
//! Version identity: every dataset is registered in
//! `derived_dataset_versions`. A dataset whose registration is absent returns
//! `absent` and **no rows**; unversioned data is never served (issue-391 §9.3).
//! `market.capital_events` registers one set per entity (PR-2);
//! `market.security_master` and `market.price_limit_rules` register one
//! whole-dataset row. The whole-market interval datasets (`security_names`,
//! `risk_warnings`, `suspensions`) register one row per dataset kind and are
//! read by that kind here.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use rusqlite::Connection;

use super::{AvailabilityStats, derived_manifest, derived_reference, invalid, series_timestamp};
use crate::data_supply::Access;
use crate::data_supply::query::{QueryPlan, QuerySnapshot};
use crate::data_supply::wire::*;
use crate::market_storage::Error;
use crate::research::temporal::{Temporal, TemporalReport};
use crate::security_master::assembly;
use crate::sqlite_market::{derived, evidence_store, rules, security};

/// The dataset's `derived_dataset_versions.dataset_kind` for the datasets that
/// register one whole-market row. `capital_events` is per entity and handled
/// separately.
fn global_kind(dataset: Dataset) -> Option<&'static str> {
    match dataset {
        Dataset::SecurityMaster => Some(assembly::DATASET_KIND),
        Dataset::SecurityNames => Some("security_names"),
        Dataset::RiskWarnings => Some("risk_warnings"),
        Dataset::Suspensions => Some("suspensions"),
        Dataset::PriceLimitRules => Some(crate::trading_rules::DATASET_KIND),
        Dataset::CapitalEvents => None,
        _ => None,
    }
}

/// The registration of the dataset as selected: per entity for capital
/// events, one dataset row otherwise.
fn registration(
    c: &Connection,
    dataset: Dataset,
    entity: Option<&Security>,
) -> Result<Option<derived::DerivedVersionRow>, Error> {
    if dataset == Dataset::CapitalEvents {
        let entity = entity.ok_or_else(|| invalid("capital events require an entity"))?;
        return derived::read_version(c, security::CAPITAL_EVENTS_KIND, &entity.symbol());
    }
    if dataset == Dataset::PriceLimitRules {
        return derived::read_version(
            c,
            crate::trading_rules::DATASET_KIND,
            crate::trading_rules::DATASET_KEY,
        );
    }
    let Some(kind) = global_kind(dataset) else {
        return Ok(None);
    };
    derived::read_kind_version(c, kind)
}

/// The `temporal` block for one answer (issue-391 §12.2). A dataset whose
/// declaration says `known_at_coverage: none` is named even when it has zero
/// rows, so the caller never reads "no rows" as "no limitation".
pub(super) fn temporal_block(dataset: Dataset, report: &TemporalReport) -> TemporalBlock {
    let mut without: Vec<String> = report
        .datasets_without_known_at()
        .into_iter()
        .map(str::to_owned)
        .collect();
    let declared = dataset.temporal();
    if declared.known_at_coverage == TemporalCoverage::None
        && let Some(name) = dataset.temporal_name()
        && !without.iter().any(|seen| seen.as_str() == name)
    {
        without.push(name.to_owned());
    }
    TemporalBlock {
        as_of: report.as_of,
        known_at: report.known_at,
        unknown_known_at_rows: report.unknown_known_at_rows(),
        datasets_without_known_at: without,
    }
}

fn date(day: Option<NaiveDate>) -> FactValue {
    FactValue::day(day)
}

fn text(value: impl Into<String>) -> FactValue {
    FactValue::Text(value.into())
}

fn int(value: i64) -> FactValue {
    FactValue::Int(value)
}

fn flag(value: bool) -> FactValue {
    FactValue::Bool(value)
}

fn cells(pairs: Vec<(&'static str, FactValue)>) -> BTreeMap<String, FactValue> {
    pairs
        .into_iter()
        .map(|(field, value)| (field.to_owned(), value))
        .collect()
}

/// Interval row admission: the range window and the `as_of` record set must
/// both admit the row (issue-391 §12.2). `as_of: None` is a history read
/// (`get_security_history`): the window alone decides, so every interval that
/// overlaps it is admitted. `query_data` always passes `Some`, which keeps the
/// original two-part test.
fn interval_hits(
    from: NaiveDate,
    to: Option<NaiveDate>,
    start: NaiveDate,
    end: NaiveDate,
    as_of: Option<NaiveDate>,
) -> bool {
    let in_window = from <= end && to.is_none_or(|to| to > start);
    let effective = as_of.is_none_or(|as_of| from <= as_of && to.is_none_or(|to| as_of < to));
    in_window && effective
}

/// The record-set day of a dataset that has no history mode. `QueryPlan::new`
/// only lets `Filter::History` reach the datasets that do, so `None` here is a
/// plan bug and is reported rather than guessed.
fn point_in_time(report: &TemporalReport, dataset: Dataset) -> Result<NaiveDate, Error> {
    report.as_of.ok_or_else(|| {
        invalid(&format!(
            "{} has no history mode; it needs an as_of day",
            dataset.name()
        ))
    })
}

fn point_hits(day: NaiveDate, start: NaiveDate, end: NaiveDate, as_of: NaiveDate) -> bool {
    day >= start && day <= end && day <= as_of
}

fn suspension_hits(
    row: &crate::security_master::SuspensionInterval,
    start: NaiveDate,
    end: NaiveDate,
    as_of: Option<NaiveDate>,
) -> bool {
    row.start_day <= end && row.end_day >= start && as_of.is_none_or(|as_of| row.covers(as_of))
}

fn entity(entity: &str) -> Result<Security, Error> {
    Security::parse(entity).map_err(Error::Integrity)
}

/// Read one M2 dataset for `query_data` v2.
pub(super) fn query(
    c: &Connection,
    plan: &QueryPlan,
    access: &Access,
) -> Result<QuerySnapshot, Error> {
    let dataset = plan.args.dataset;
    let Some(temporal) = plan.temporal else {
        return Err(invalid("M2 dataset without a resolved temporal plan"));
    };
    let (start, end) = match &plan.args.filter {
        Filter::Daily {
            start_date,
            end_date,
            ..
        }
        | Filter::History {
            start_date,
            end_date,
        } => (*start_date, *end_date),
        _ => return Err(invalid("M2 datasets require filter.kind=daily")),
    };
    let mut report = match temporal.as_of {
        Some(as_of) => TemporalReport::new(Temporal {
            as_of,
            known_at: temporal.known_at,
        }),
        None => TemporalReport::history(temporal.known_at),
    };
    let mut result = QuerySnapshot {
        manifest: vec![],
        rows: vec![],
        temporal: None,
        limitations: vec![],
    };
    match dataset {
        Dataset::SecurityMaster => {
            security_master(c, plan, access, start, end, &mut report, &mut result)?
        }
        Dataset::CapitalEvents => capital_events(c, plan, start, end, &mut report, &mut result)?,
        Dataset::RiskWarnings => {
            risk_warnings(c, plan, access, start, end, &mut report, &mut result)?
        }
        Dataset::Suspensions => suspensions(c, plan, access, start, end, &mut report, &mut result)?,
        Dataset::PriceLimitRules => price_limit_rules(c, start, end, &mut report, &mut result)?,
        Dataset::SecurityNames => {
            security_names(c, plan, access, start, end, &mut report, &mut result)?
        }
        _ => return Err(invalid("dataset is not an M2 fact dataset")),
    }
    result.temporal = Some(temporal_block(dataset, &report));
    Ok(result)
}

fn security_master(
    c: &Connection,
    plan: &QueryPlan,
    access: &Access,
    start: NaiveDate,
    end: NaiveDate,
    report: &mut TemporalReport,
    result: &mut QuerySnapshot,
) -> Result<(), Error> {
    let registration = registration(c, Dataset::SecurityMaster, None)?;
    result.manifest.push(derived_or_absent(
        c,
        Dataset::SecurityMaster,
        None,
        registration.as_ref(),
        "assembled from TDX facts and admitted exchange lists; 知晓时间 is per row (§12.2)",
    )?);
    if registration.is_none() {
        note_unregistered(result, Dataset::SecurityMaster);
        return Ok(());
    }
    result.limitations.push(
        "known_at is stored only where a source supplies one and is NULL otherwise, never proxied by the effective day"
            .into(),
    );
    let as_of = Some(point_in_time(report, Dataset::SecurityMaster)?);
    let selected: Option<BTreeSet<String>> = match &plan.args.selection {
        Selection::Market { .. } => None,
        _ => Some(plan.entities.iter().map(Security::symbol).collect()),
    };
    for fact in security::read_security_master(c)? {
        let entity = entity(&fact.entity())?;
        if selected
            .as_ref()
            .is_some_and(|selected| !selected.contains(&entity.symbol()))
        {
            continue;
        }
        if !access.allows(Dataset::SecurityMaster, Some(&entity)) {
            continue;
        }
        // Information set first, record set second (issue-391 §12.2): a NULL
        // 知晓时间 row is counted even when the effective window excludes it.
        if !report.observe("security_master", fact.known_at) {
            continue;
        }
        if !interval_hits(fact.effective_from, fact.effective_to, start, end, as_of) {
            continue;
        }
        let mut flags = Vec::new();
        if fact.listing_conflict {
            flags.push("listing_conflict");
        }
        if fact.delisting_conflict {
            flags.push("delisting_conflict");
        }
        result.rows.push(Row::Fact {
            entity: Some(entity),
            day: fact.effective_from,
            cells: cells(vec![
                ("effective_from", date(Some(fact.effective_from))),
                ("effective_to", date(fact.effective_to)),
                ("instrument_class", int(i64::from(fact.instrument_class))),
                (
                    "issuer_key",
                    fact.issuer_key.clone().map_or(FactValue::Null, text),
                ),
                ("issuer_resolution", text(fact.issuer_resolution.as_str())),
                ("listing_date", date(fact.listing_date)),
                ("listing_source", text(fact.listing_source.as_str())),
                ("delisting_date", date(fact.delisting_date)),
                ("delisting_source", text(fact.delisting_source.as_str())),
                ("arrangement_start", date(fact.arrangement_start)),
                ("arrangement_end", date(fact.arrangement_end)),
                ("known_at", date(fact.known_at)),
                ("flags", FactValue::Text(flags.join(","))),
            ]),
        });
        check_budget(result)?;
    }
    pin_is_current(plan, registration.as_ref(), None)?;
    Ok(())
}

/// `market.security_names`: the SZSE dated name intervals (D-61). The writer
/// (`derived:security_names`, #446 PR-12) replaces the whole table and
/// registers one version; this reader applies the same entity filter,
/// `known_at` information set and half-open interval window as
/// `market.risk_warnings` (`issue-391` §12.2).
fn security_names(
    c: &Connection,
    plan: &QueryPlan,
    access: &Access,
    start: NaiveDate,
    end: NaiveDate,
    report: &mut TemporalReport,
    result: &mut QuerySnapshot,
) -> Result<(), Error> {
    let registration = registration(c, Dataset::SecurityNames, None)?;
    result.manifest.push(derived_or_absent(
        c,
        Dataset::SecurityNames,
        None,
        registration.as_ref(),
        "coverage: effective-time name history is available from the source start \
         (SZSE SSGSGMXX/tab2, 1994-01-03); knowledge-time PIT (known_at filtering) \
         is available from the first capture day. known_at is the Shanghai day on \
         which the first snapshot containing the change row was stored, never the \
         change day; SH/BJ have no dated name source (D-61), so their absence is a \
         coverage window and is not back-filled from a current name",
    )?);
    if registration.is_none() {
        // No registration means the dataset was never written (or a quiet,
        // empty source removed it); `absent` with zero rows is the answer, and
        // the read never invents a current name to fill the gap.
        note_unregistered(result, Dataset::SecurityNames);
        return Ok(());
    }
    let as_of = report.as_of;
    let selected: BTreeSet<String> = plan.entities.iter().map(Security::symbol).collect();
    for row in security::read_security_names(c)? {
        // D-76: ETF observed names (`exchange_snapshot`) are written by PR-2
        // but have no read surface yet. PR-3 owns it, together with the
        // `limitations` text that says what their `effective_from` means; until
        // then this reader answers exactly what it answered before.
        if row.source_kind == crate::security_master::names::NameSourceKind::ExchangeSnapshot {
            continue;
        }
        let entity = entity(&format!("{}.{}", row.exchange, row.code))?;
        if !selected.contains(&entity.symbol())
            || !access.allows(Dataset::SecurityNames, Some(&entity))
        {
            continue;
        }
        // Information set first, record set second (`issue-391` §12.2): a NULL
        // known_at row is counted even when a later step excludes the interval.
        if !report.observe("security_names", row.known_at) {
            continue;
        }
        if !interval_hits(row.effective_from, row.effective_to, start, end, as_of) {
            continue;
        }
        result.rows.push(Row::Fact {
            entity: Some(entity),
            day: row.effective_from,
            cells: cells(vec![
                ("effective_from", date(Some(row.effective_from))),
                ("effective_to", date(row.effective_to)),
                ("name", text(row.name)),
                ("certainty", text(row.certainty.as_str())),
                ("known_at", date(row.known_at)),
                ("source_kind", text(row.source_kind.as_str())),
            ]),
        });
        check_budget(result)?;
    }
    Ok(())
}

fn capital_events(
    c: &Connection,
    plan: &QueryPlan,
    start: NaiveDate,
    end: NaiveDate,
    report: &mut TemporalReport,
    result: &mut QuerySnapshot,
) -> Result<(), Error> {
    let as_of = point_in_time(report, Dataset::CapitalEvents)?;
    for entity in &plan.entities {
        let registration = registration(c, Dataset::CapitalEvents, Some(entity))?;
        result.manifest.push(derived_or_absent(
            c,
            Dataset::CapitalEvents,
            Some(entity),
            registration.as_ref(),
            "parsed from gbbq; the source carries no announcement day, so known_at is NULL (BR-M1-9)",
        )?);
        if registration.is_none() {
            note_unregistered(result, Dataset::CapitalEvents);
            continue;
        }
        for event in security::read_capital_events(c, &entity.symbol())? {
            if !report.observe("capital_events", event.known_at) {
                continue;
            }
            if !point_hits(event.effective_day, start, end, as_of) {
                continue;
            }
            result.rows.push(Row::Fact {
                entity: Some((*entity).clone()),
                day: event.effective_day,
                cells: cells(vec![
                    ("ordinal", int(event.ordinal)),
                    ("effective_day", date(Some(event.effective_day))),
                    ("category", int(i64::from(event.category))),
                    ("float_after", int(event.float_after)),
                    ("total_after", int(event.total_after)),
                    ("known_at", date(event.known_at)),
                    ("suspicious", flag(event.suspicious)),
                ]),
            });
            check_budget(result)?;
        }
        pin_is_current(plan, registration.as_ref(), Some(entity))?;
    }
    Ok(())
}

fn risk_warnings(
    c: &Connection,
    plan: &QueryPlan,
    access: &Access,
    start: NaiveDate,
    end: NaiveDate,
    report: &mut TemporalReport,
    result: &mut QuerySnapshot,
) -> Result<(), Error> {
    let registration = registration(c, Dataset::RiskWarnings, None)?;
    // A registered version describes itself from its own stored evidence
    // (D-58 coverage windows and row counts); an unregistered dataset has no
    // metadata at all and says so in `limitations`.
    let described = match registration.as_ref() {
        Some(registration) => Some(risk_warning_description(c, registration)?),
        None => None,
    };
    result.manifest.push(derived_or_absent(
        c,
        Dataset::RiskWarnings,
        None,
        registration.as_ref(),
        described
            .as_ref()
            .map_or("", |described| described.coverage_reason.as_str()),
    )?);
    let Some(described) = described else {
        note_unregistered(result, Dataset::RiskWarnings);
        return Ok(());
    };
    result.limitations.extend(described.limitations);
    let as_of = report.as_of;
    let selected: BTreeSet<String> = plan.entities.iter().map(Security::symbol).collect();
    for row in security::read_risk_warning_intervals(c)? {
        let entity = entity(&row.entity())?;
        if !selected.contains(&entity.symbol())
            || !access.allows(Dataset::RiskWarnings, Some(&entity))
        {
            continue;
        }
        if !report.observe("risk_warnings", row.known_at) {
            continue;
        }
        if !interval_hits(row.effective_from, row.effective_to, start, end, as_of) {
            continue;
        }
        result.rows.push(Row::Fact {
            entity: Some(entity),
            day: row.effective_from,
            cells: cells(vec![
                ("effective_from", date(Some(row.effective_from))),
                ("effective_to", date(row.effective_to)),
                ("normalized_status", text(row.status.as_str())),
                ("raw_status_code", text(row.raw_status_code.clone())),
                (
                    "announced_at",
                    row.announced_at.clone().map_or(FactValue::Null, text),
                ),
                ("known_at", date(row.known_at)),
                ("source_kind", text(row.source_kind.clone())),
            ]),
        });
        check_budget(result)?;
    }
    Ok(())
}

fn suspensions(
    c: &Connection,
    plan: &QueryPlan,
    access: &Access,
    start: NaiveDate,
    end: NaiveDate,
    report: &mut TemporalReport,
    result: &mut QuerySnapshot,
) -> Result<(), Error> {
    let registration = registration(c, Dataset::Suspensions, None)?;
    result.manifest.push(derived_or_absent(
        c,
        Dataset::Suspensions,
        None,
        registration.as_ref(),
        "SH `confirmed` rows come from the SSE suspension/resumption table since 2008 (D-60); \
         every other gap is a derived `unresolved_gap` (AC-M2-8)",
    )?);
    if registration.is_none() {
        note_unregistered(result, Dataset::Suspensions);
        return Ok(());
    }
    result.limitations.push(
        "`unresolved_gap` means a trading day without a bar for unknown reason, never `suspended`; \
         SZ has no `confirmed` rows and SH has none before 2008 (D-60), so tradability there is `unknown`; \
         intraday halts are not recorded; unresolved_gap rows have known_at NULL, so a known_at filter excludes them"
            .into(),
    );
    let as_of = report.as_of;
    let selected: BTreeSet<String> = plan.entities.iter().map(Security::symbol).collect();
    for row in security::read_suspension_intervals(c)? {
        let entity = entity(&row.entity())?;
        if !selected.contains(&entity.symbol())
            || !access.allows(Dataset::Suspensions, Some(&entity))
        {
            continue;
        }
        if !report.observe("suspensions", row.known_at) {
            continue;
        }
        if !suspension_hits(&row, start, end, as_of) {
            continue;
        }
        result.rows.push(Row::Fact {
            entity: Some(entity),
            day: row.start_day,
            cells: cells(vec![
                ("start_day", date(Some(row.start_day))),
                ("end_day", date(Some(row.end_day))),
                ("certainty", text(row.certainty.as_str())),
                ("missing_days", int(row.missing_days)),
                ("reason", text(row.reason.as_str())),
                ("known_at", date(row.known_at)),
            ]),
        });
        check_budget(result)?;
    }
    Ok(())
}

fn price_limit_rules(
    c: &Connection,
    start: NaiveDate,
    end: NaiveDate,
    report: &mut TemporalReport,
    result: &mut QuerySnapshot,
) -> Result<(), Error> {
    let registration = registration(c, Dataset::PriceLimitRules, None)?;
    result.manifest.push(derived_or_absent(
        c,
        Dataset::PriceLimitRules,
        None,
        registration.as_ref(),
        "effective-dated rule rows (effective_to is exclusive); this dataset returns the rules, not derived limit prices",
    )?);
    if registration.is_none() {
        note_unregistered(result, Dataset::PriceLimitRules);
        return Ok(());
    }
    result.limitations.push(
        "no rule row starts before 1996-12-16 (NOT_AVAILABLE, D-44), so earlier dates are `unknown`, never `no limit`; \
         the first-day 44%/36% ranges (SH from 2013-12-13, SZ from 2014-06-13, until 2023-02-16) are not encoded (D-64); \
         rule rows carry no known_at"
            .into(),
    );
    let as_of = Some(point_in_time(report, Dataset::PriceLimitRules)?);
    for rule in rules::read_trading_rules(c)?.price_limits {
        // Rule rows carry no 知晓时间 (§12.4 export columns); observing `None`
        // is what makes the temporal block report `known_at_coverage: none`.
        if !report.observe("price_limit_rules", None) {
            continue;
        }
        if !interval_hits(rule.effective_from, rule.effective_to, start, end, as_of) {
            continue;
        }
        result.rows.push(Row::Fact {
            entity: None,
            day: rule.effective_from,
            cells: cells(vec![
                ("rule_id", text(rule.rule_id)),
                ("market", text(rule.market.as_str())),
                ("board", text(rule.board.as_str())),
                ("status_class", text(rule.status_class.as_str())),
                ("listing_phase", text(rule.listing_phase.as_str())),
                ("band_bp", rule.band_bp.map_or(FactValue::Null, int)),
                (
                    "min_band_ticks",
                    rule.min_band_ticks.map_or(FactValue::Null, int),
                ),
                ("band_side", text(rule.band_side.as_str())),
                ("rounding", text(rule.rounding.as_str())),
                ("effective_from", date(Some(rule.effective_from))),
                ("effective_to", date(rule.effective_to)),
                ("basis_ref", text(rule.basis_ref)),
                ("basis_status", text(rule.basis_status.as_str())),
            ]),
        });
        check_budget(result)?;
    }
    Ok(())
}

/// What a registered `market.risk_warnings` version says about itself.
struct RiskWarningDescription {
    coverage_reason: String,
    limitations: Vec<String>,
}

/// Describe a registered risk-warning version from its stored evidence (the
/// row counts and acquisition windows the writer recorded) plus the D-58
/// source limits. Nothing here depends on the dataset being "pending": the
/// registration exists, so the source was admitted and written.
fn risk_warning_description(
    c: &Connection,
    registration: &derived::DerivedVersionRow,
) -> Result<RiskWarningDescription, Error> {
    let (_, canonical) = evidence_store::read_evidence(c, &registration.evidence_digest)?;
    let evidence: serde_json::Value = serde_json::from_str(&canonical)
        .map_err(|error| Error::Integrity(format!("risk_warnings evidence: {error}")))?;
    // Keys are those `replace_risk_warnings` writes into the registered
    // evidence (pinned by `registered_evidence_has_the_keys_the_reader_uses`).
    let count = |key: &str| evidence[key].as_u64();
    let mut parts = Vec::new();
    if let (Some(sz), Some(sh)) = (count("sz_rows"), count("sh_rows")) {
        parts.push(format!(
            "{sz} SZ and {sh} SH stored ST interval row(s) (SH intervals are built from announcement events)"
        ));
    }
    if let (Some(observed), Some(not_observed)) =
        (count("observed_windows"), count("not_observed_windows"))
    {
        parts.push(format!(
            "{observed} acquisition window(s) observed, {not_observed} not_observed"
        ));
    }
    let coverage_reason = format!(
        "{}{}a day without a covering row is `unknown`, never `normal` (D-58)",
        parts.join("; "),
        if parts.is_empty() { "" } else { "; " }
    );

    let mut limitations = vec![
        "SZ rows are ST intervals from the SZSE short-name table (source start 1994-01-03); \
         SH rows are announcement events (implement/revoke) whose effective day is the announcement day (OWNER_RISK_ACCEPTED), \
         not name intervals; SH coverage starts no earlier than 2000-01 (the SSE archive start) and BJ is not available (D-58)"
            .to_owned(),
        "a day without a covering row is `unknown`, never `normal`: absence of an announcement is not absence of ST (D-58)"
            .to_owned(),
    ];
    // Windows the writer could not observe (e.g. the SSE archive starts in
    // 2000-01 but returns nothing for 2000-2003) are listed from the stored
    // evidence, so the answer states the real SH start instead of the archive's.
    let mut gaps: BTreeMap<String, (usize, String, String)> = BTreeMap::new();
    for window in evidence["coverage"].as_array().into_iter().flatten() {
        if window["observed"].as_bool() != Some(false) {
            continue;
        }
        let (Some(exchange), Some(from), Some(to)) = (
            window["exchange"].as_str(),
            window["from"].as_str(),
            window["to"].as_str(),
        ) else {
            continue;
        };
        let entry = gaps
            .entry(exchange.to_owned())
            .or_insert_with(|| (0, from.to_owned(), to.to_owned()));
        entry.0 += 1;
        if from < entry.1.as_str() {
            entry.1 = from.to_owned();
        }
        if to > entry.2.as_str() {
            entry.2 = to.to_owned();
        }
    }
    if !gaps.is_empty() {
        let listed = gaps
            .iter()
            .map(|(exchange, (windows, from, to))| {
                format!("{exchange}: {windows} window(s) between {from} and {to}")
            })
            .collect::<Vec<_>>()
            .join("; ");
        limitations.push(format!(
            "windows with no usable source pages are not_observed and stay `unknown`: {listed}"
        ));
    }
    Ok(RiskWarningDescription {
        coverage_reason,
        limitations,
    })
}

/// An unregistered dataset has no metadata, so the reason for an empty answer
/// travels in `limitations`.
fn note_unregistered(result: &mut QuerySnapshot, dataset: Dataset) {
    let note = format!(
        "{} has no registered version: it has not been written yet, so zero rows means `unknown`, not that no such fact exists",
        dataset.name()
    );
    if !result.limitations.contains(&note) {
        result.limitations.push(note);
    }
}

fn derived_or_absent(
    c: &Connection,
    dataset: Dataset,
    entity: Option<&Security>,
    registration: Option<&derived::DerivedVersionRow>,
    coverage_reason: &str,
) -> Result<ManifestEntry, Error> {
    let Some(registration) = registration else {
        return Ok(super::absent(entity));
    };
    derived_manifest(c, dataset, entity, registration, coverage_reason)
}

/// An explicit pin must name the current registration, otherwise the request
/// is stale rather than silently answered from a newer set.
fn pin_is_current(
    plan: &QueryPlan,
    registration: Option<&derived::DerivedVersionRow>,
    entity: Option<&Security>,
) -> Result<(), Error> {
    let VersionChoice::ExplicitVersionMap { versions } = &plan.args.version else {
        return Ok(());
    };
    let pinned = match entity {
        Some(entity) => versions
            .iter()
            .find(|v| &v.entity == entity)
            .map(|v| v.version_id.as_str()),
        None => versions.first().map(|v| v.version_id.as_str()),
    };
    let current = registration.map(|r| r.version_id.as_str());
    if pinned != current {
        return Err(Error::Stale("M2 dataset changed".into()));
    }
    Ok(())
}

pub(super) fn check_budget(result: &QuerySnapshot) -> Result<(), Error> {
    if result.rows.len() > crate::data_supply::query::MAX_ROWS {
        return Err(invalid("snapshot row budget exceeded"));
    }
    Ok(())
}

/// Version identities for `list_data_versions` v2. `capital_events` has one
/// registration per selected entity; the other datasets have one row.
pub(super) fn versions(
    c: &Connection,
    dataset: Dataset,
    entities: &[Security],
) -> Result<Vec<VersionInfo>, Error> {
    let mut out: Vec<VersionInfo> = Vec::new();
    if dataset == Dataset::CapitalEvents {
        for entity in entities {
            let Some(registration) = registration(c, dataset, Some(entity))? else {
                continue;
            };
            out.push(VersionInfo {
                version_ref: derived_reference(dataset, Some(entity), &registration),
                published_at: series_timestamp(&registration.updated_at)?,
            });
        }
        return Ok(out);
    }
    if let Some(registration) = registration(c, dataset, None)? {
        out.push(VersionInfo {
            version_ref: derived_reference(dataset, None, &registration),
            published_at: series_timestamp(&registration.updated_at)?,
        });
    }
    Ok(out)
}

/// `get_data_availability` facts for one entity, derived from the same
/// snapshot `query` returned (one read for the whole selection).
pub(super) fn stats(snapshot: &QuerySnapshot, entity: Option<&Security>) -> AvailabilityStats {
    let days = snapshot.rows.iter().filter_map(|row| {
        let Row::Fact {
            entity: row_entity,
            day,
            ..
        } = row
        else {
            return None;
        };
        match (entity, row_entity.as_ref()) {
            (Some(entity), Some(row_entity)) if entity == row_entity => Some(*day),
            (None, _) => Some(*day),
            _ => None,
        }
    });
    let mut first = None;
    let mut last = None;
    let mut count = 0;
    for day in days {
        first = Some(first.map_or(day, |first: NaiveDate| first.min(day)));
        last = Some(last.map_or(day, |last: NaiveDate| last.max(day)));
        count += 1;
    }
    let manifest = snapshot
        .manifest
        .iter()
        .find(|entry| entry.entity.as_ref() == entity)
        .or_else(|| snapshot.manifest.first())
        .cloned();
    (manifest, first, last, count)
}

/// The current version reference of one M2 dataset, used by the snapshot
/// re-verification loop.
pub(super) fn version_ref(c: &Connection, reference: &VersionRef) -> Result<VersionRef, Error> {
    let registration = registration(c, reference.dataset, reference.entity.as_ref())?
        .ok_or_else(|| Error::Integrity("M2 dataset registration vanished".into()))?;
    Ok(derived_reference(
        reference.dataset,
        reference.entity.as_ref(),
        &registration,
    ))
}

/// The M2 `as_of` default: the last trading day on or before `today`
/// (issue-391 §12.2). Without a registered calendar the adapter uses `today`
/// itself rather than inventing a trading day.
pub(super) fn last_trading_day_upto(c: &Connection, today: NaiveDate) -> Result<NaiveDate, Error> {
    let registered = derived::read_version(
        c,
        crate::trading_calendar::DATASET_KIND,
        crate::trading_calendar::DATASET_KEY,
    )?;
    if registered.is_none() {
        return Ok(today);
    }
    let day: Option<i64> = c
        .query_row(
            "SELECT MAX(trade_day) FROM trading_days WHERE trade_day <= ?1",
            rusqlite::params![crate::compact_market::epoch_day(today)],
            |row| row.get(0),
        )
        .map_err(|error| Error::Storage(error.to_string()))?;
    let day = day
        .map(crate::compact_market::epoch_date)
        .transpose()
        .map_err(|error| Error::Integrity(error.to_string()))?;
    Ok(day.unwrap_or(today))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    /// `as_of` selects the record set: an interval is effective from
    /// `effective_from` (inclusive) until `effective_to` (exclusive), and the
    /// window is a half-open overlap test.
    #[test]
    fn as_of_and_window_are_half_open() {
        let from = day(2020, 1, 1);
        let to = Some(day(2020, 2, 1));
        assert!(interval_hits(
            from,
            to,
            from,
            day(2020, 12, 31),
            Some(day(2020, 1, 15))
        ));
        assert!(!interval_hits(
            from,
            to,
            from,
            day(2020, 12, 31),
            Some(day(2020, 2, 1))
        ));
        assert!(!interval_hits(
            from,
            to,
            from,
            day(2020, 12, 31),
            Some(day(2019, 12, 31))
        ));
        // A window that starts exactly at `to` does not overlap.
        assert!(!interval_hits(
            from,
            to,
            day(2020, 2, 1),
            day(2020, 12, 31),
            Some(day(2020, 1, 15))
        ));
        // A point fact after `as_of` is not in the record set.
        assert!(!point_hits(
            day(2020, 3, 1),
            from,
            day(2020, 12, 31),
            day(2020, 2, 1)
        ));
    }

    /// A history read (`as_of: None`) admits every interval that overlaps the
    /// window, whatever day it was in effect; the window test is unchanged.
    #[test]
    fn history_admits_every_interval_overlapping_the_window() {
        let (start, end) = (day(2020, 3, 1), day(2020, 6, 30));
        let hits = |from, to| interval_hits(from, to, start, end, None);
        // Ended before the window, starts after it: not admitted.
        assert!(!hits(day(2020, 1, 1), Some(day(2020, 3, 1))));
        assert!(!hits(day(2020, 7, 1), None));
        // Overlapping the window at either edge, inside it, or around it.
        assert!(hits(day(2020, 1, 1), Some(day(2020, 3, 2))));
        assert!(hits(day(2020, 6, 30), None));
        assert!(hits(day(2020, 4, 1), Some(day(2020, 5, 1))));
        assert!(hits(day(2019, 1, 1), None));
        // With a point-in-time day the same intervals are narrowed to it.
        assert!(!interval_hits(
            day(2020, 4, 1),
            Some(day(2020, 5, 1)),
            start,
            end,
            Some(day(2020, 6, 1))
        ));
    }

    /// A dataset that never carries a 知晓时间 is named in the answer even when
    /// it contributed zero rows (issue-391 §12.2).
    #[test]
    fn none_coverage_is_named_with_zero_rows() {
        let report = TemporalReport::new(Temporal::known_by(day(2020, 1, 2), day(2020, 1, 1)));
        let block = temporal_block(Dataset::CapitalEvents, &report);
        assert_eq!(block.as_of, Some(day(2020, 1, 2)));
        assert_eq!(block.known_at, Some(day(2020, 1, 1)));
        assert_eq!(block.unknown_known_at_rows, 0);
        assert_eq!(block.datasets_without_known_at, vec!["capital_events"]);
    }
}
