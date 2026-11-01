//! Exchange-source refresh runs (M3 R2, `docs/tasks/issue-446-exchange-refresh.md`
//! §5.5/§5.6, D-66 Q1/Q2/Q3/Q5).
//!
//! This is the orchestration that was missing between the shared acquisition
//! entry and the source-store index: a `tail` looks at the open and previous
//! natural quarter (plus cheap whole tables and first-page probes), a `full`
//! looks at every fingerprint of the selected sources. The run is recorded in
//! `source_refresh_runs` / `source_refresh_run_members` so R3's replay can
//! select pages by run membership instead of re-deriving "latest" per
//! fingerprint, and so `verified_through` survives a restart or task expiry.
//!
//! It never publishes a market dataset and never writes a derived item: closed
//! windows with revisions are picked up by the next daily update (D-66 Q7).

pub mod plan;
pub mod wire;

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;

use crate::exchange_source::{
    AcquireError, AcquisitionOutcome, Client, Entry, FrozenSnapshot, SnapshotRow, SourceId,
    TransportError, validate,
};
use crate::market_storage::Error as StorageError;
use crate::security_master::risk_warnings::SzNameChange;
use crate::source_store::{SourceStore, StoreError, Unconfigured};
use crate::sqlite_market::backend::MarketBackend;
use crate::sqlite_market::refresh_runs::{RunFinish, RunMemberRow, RunSourceRow, RunStart};

use self::plan::{PlanClass, SourcePlan};
use self::wire::{
    RefreshFailure, RefreshMode, RefreshReport, RefreshRequest, RefreshStatus, Revision,
    SourceReport,
};

#[derive(Debug, thiserror::Error)]
pub enum RefreshError {
    #[error("invalid_arguments: {0}")]
    Invalid(String),
    #[error(transparent)]
    Unconfigured(#[from] Unconfigured),
    #[error(transparent)]
    Acquire(#[from] AcquireError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Database(#[from] StorageError),
    #[error(transparent)]
    Transport(#[from] TransportError),
}

impl RefreshError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "invalid_arguments",
            Self::Unconfigured(error) => error.code(),
            Self::Acquire(error) => error.code(),
            Self::Store(error) => error.code(),
            Self::Database(_) => "storage_error",
            Self::Transport(error) => error.code(),
        }
    }
}

/// Run one refresh and persist its record. `request.dry_run` must be false;
/// call [`dry_run`] for the no-I/O planning response.
pub fn run(
    store: &SourceStore,
    backend: &MarketBackend,
    client: &Client,
    today: NaiveDate,
    request: &RefreshRequest,
) -> Result<RefreshReport, RefreshError> {
    let plans =
        plan::build(request.mode, today, &request.sources).map_err(RefreshError::Invalid)?;
    if request.dry_run {
        return Ok(dry_run_report(request, plans));
    }
    let previous = PreviousSnapshots::load(backend)?;
    let started_at = crate::exchange_source::freeze::now();
    let run_id = format!(
        "refresh-{}-{}",
        request.mode.as_str(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );
    backend.begin_source_refresh_run(&RunStart {
        run_id: run_id.clone(),
        mode: request.mode.as_str().to_string(),
        scope: plans
            .iter()
            .map(|plan| plan.source_id.as_str())
            .collect::<Vec<_>>()
            .join(","),
        started_at: started_at.clone(),
        task_id: None,
    })?;

    let entry = Entry::new(store, backend, client).with_today(today);
    let mut accumulator = Accumulator::default();
    let mut sources = Vec::with_capacity(plans.len());
    for source in &plans {
        let requests_before = client.attempts();
        let mut stats = SourceStats::new(source, request.mode);
        match refresh_source(
            &entry,
            store,
            &previous,
            source,
            &mut accumulator,
            &mut stats,
        ) {
            Ok(()) => {}
            Err(error) => {
                // A source-level fault (store/index/window helper) still leaves
                // a failed run row: the record must explain why the run did not
                // complete instead of looking like it never started.
                stats.failed += 1;
                stats.complete = false;
                accumulator.failures.push(RefreshFailure {
                    source_id: source.source_id.as_str().to_string(),
                    window_start: None,
                    window_end: None,
                    code: error.code().to_string(),
                    detail: error.to_string(),
                });
            }
        }
        stats.requests = client.attempts().saturating_sub(requests_before);
        sources.push(stats.into_report(source));
    }

    let failed = !accumulator.failures.is_empty();
    let status = if failed {
        RefreshStatus::Failed
    } else {
        RefreshStatus::Succeeded
    };
    // The run-level date is only meaningful when at least one source was
    // observed as a complete set: a tail scoped to one window source verifies
    // no complete set and must not claim a date. Per-source rows carry the
    // finer truth, and `latest_complete_verified_through` reads those.
    let any_complete = sources.iter().any(|source| source.complete);
    let verified_through = if !failed && any_complete {
        Some(today.to_string())
    } else {
        None
    };
    if !failed {
        for source in &mut sources {
            if source.complete {
                source.verified_through = Some(today.to_string());
            }
        }
    }
    let finished_at = crate::exchange_source::freeze::now();
    let source_rows = sources
        .iter()
        .map(|source| {
            let plan = plans
                .iter()
                .find(|plan| plan.source_id.as_str() == source.source_id.as_str())
                .expect("one plan per source report");
            RunSourceRow {
                run_id: run_id.clone(),
                source_id: source.source_id.clone(),
                class: plan.class.as_str().to_string(),
                complete: source.complete,
                requests: source.requests,
                snapshots: source.snapshots as u64,
                changed: source.changed as u64,
                unobserved_windows: source.unobserved_windows as u64,
                failed_windows: source.failed_windows as u64,
                verified_through: source.verified_through.clone(),
            }
        })
        .collect::<Vec<_>>();
    let members = accumulator
        .members
        .into_values()
        .map(|mut member| {
            member.run_id = run_id.clone();
            member
        })
        .collect::<Vec<_>>();
    backend.finish_source_refresh_run(
        &run_id,
        &RunFinish {
            status: match status {
                RefreshStatus::Succeeded => "succeeded",
                RefreshStatus::Failed => "failed",
                RefreshStatus::Planned => unreachable!("a real run is never planned"),
            }
            .to_string(),
            finished_at: finished_at.clone(),
            requests: sources.iter().map(|source| source.requests).sum(),
            reused: sources.iter().map(|source| source.reused as u64).sum(),
            changed: sources.iter().map(|source| source.changed as u64).sum(),
            failed_windows: sources
                .iter()
                .map(|source| source.failed_windows as u64)
                .sum(),
            unobserved_windows: sources
                .iter()
                .map(|source| source.unobserved_windows as u64)
                .sum(),
            verified_through: verified_through.clone(),
            detail: (!accumulator.failures.is_empty())
                .then(|| serde_json::to_string(&accumulator.failures).unwrap_or_default()),
        },
        &source_rows,
        &members,
    )?;

    Ok(RefreshReport {
        run_id: Some(run_id),
        mode: request.mode,
        status,
        dry_run: false,
        started_at,
        finished_at,
        scope: plans
            .iter()
            .map(|plan| plan.source_id.as_str().to_string())
            .collect(),
        requests: sources.iter().map(|source| source.requests).sum(),
        reused: sources.iter().map(|source| source.reused).sum(),
        changed: sources.iter().map(|source| source.changed).sum(),
        failed_windows: sources.iter().map(|source| source.failed_windows).sum(),
        unobserved_windows: sources.iter().map(|source| source.unobserved_windows).sum(),
        verified_through,
        sources,
        planned: plans.iter().flat_map(SourcePlan::planned).collect(),
        revisions: accumulator.revisions,
        failures: accumulator.failures,
    })
}

/// Plan only: no HTTP, no store write, no run row.
pub fn dry_run(today: NaiveDate, request: &RefreshRequest) -> Result<RefreshReport, RefreshError> {
    let plans =
        plan::build(request.mode, today, &request.sources).map_err(RefreshError::Invalid)?;
    Ok(dry_run_report(request, plans))
}

fn dry_run_report(request: &RefreshRequest, plans: Vec<SourcePlan>) -> RefreshReport {
    let now = crate::exchange_source::freeze::now();
    let sources = plans
        .iter()
        .map(|plan| SourceReport {
            source_id: plan.source_id.as_str().to_string(),
            class: plan.class.as_str().to_string(),
            planned_units: plan.planned_units(),
            planned_windows: plan.planned_windows(),
            complete: false,
            requests: 0,
            snapshots: 0,
            reused: 0,
            changed: 0,
            unobserved_windows: 0,
            failed_windows: 0,
            verified_through: None,
        })
        .collect::<Vec<_>>();
    RefreshReport {
        run_id: None,
        mode: request.mode,
        status: RefreshStatus::Planned,
        dry_run: true,
        started_at: now.clone(),
        finished_at: now,
        scope: plans
            .iter()
            .map(|plan| plan.source_id.as_str().to_string())
            .collect(),
        requests: 0,
        reused: 0,
        changed: 0,
        failed_windows: 0,
        unobserved_windows: 0,
        verified_through: None,
        sources,
        planned: plans.iter().flat_map(SourcePlan::planned).collect(),
        revisions: Vec::new(),
        failures: Vec::new(),
    }
}

#[derive(Default)]
struct Accumulator {
    /// One member per (source, request): a probe page is not a selected page,
    /// and a later full acquisition replaces the probe's page-1 registration.
    members: BTreeMap<(String, String), RunMemberRow>,
    revisions: Vec<Revision>,
    failures: Vec<RefreshFailure>,
}

struct SourceStats {
    requests: u64,
    snapshots: usize,
    reused: usize,
    changed: usize,
    unobserved: usize,
    failed: usize,
    complete: bool,
}

impl SourceStats {
    fn new(source: &SourcePlan, mode: RefreshMode) -> Self {
        let complete = match source.class {
            PlanClass::Table => true,
            PlanClass::Window => mode == RefreshMode::Full,
            PlanClass::Probe => false,
        };
        Self {
            requests: 0,
            snapshots: 0,
            reused: 0,
            changed: 0,
            unobserved: 0,
            failed: 0,
            complete,
        }
    }

    fn into_report(self, source: &SourcePlan) -> SourceReport {
        SourceReport {
            source_id: source.source_id.as_str().to_string(),
            class: source.class.as_str().to_string(),
            planned_units: source.planned_units(),
            planned_windows: source.planned_windows(),
            complete: self.complete && self.failed == 0,
            requests: self.requests,
            snapshots: self.snapshots,
            reused: self.reused,
            changed: self.changed,
            unobserved_windows: self.unobserved,
            failed_windows: self.failed,
            // Filled by `run` after the whole run's status is known; a failed
            // run advances no source's verified_through.
            verified_through: None,
        }
    }
}

fn refresh_source(
    entry: &Entry<'_>,
    store: &SourceStore,
    previous: &PreviousSnapshots,
    source: &SourcePlan,
    accumulator: &mut Accumulator,
    stats: &mut SourceStats,
) -> Result<(), RefreshError> {
    match source.class {
        PlanClass::Table => {
            for (label, plan) in &source.plans {
                match acquire_guarded_table(entry, store, plan) {
                    Ok(outcome) => record_outcome(
                        accumulator,
                        stats,
                        previous,
                        source.source_id,
                        None,
                        &outcome,
                    ),
                    Err(error) => {
                        stats.failed += 1;
                        accumulator.failures.push(RefreshFailure {
                            source_id: source.source_id.as_str().to_string(),
                            window_start: None,
                            window_end: None,
                            code: error.code().to_string(),
                            detail: format!("{label}: {error}"),
                        });
                    }
                }
            }
        }
        PlanClass::Window => {
            for (label, plan) in &source.plans {
                for window in &source.windows {
                    match entry.acquire_window(plan, *window, source.from_param, source.to_param) {
                        Ok(outcome) => {
                            if outcome.row_count == 0 {
                                // A quiet window is not absence (BR-M3-10): the
                                // count is evidence R3 must see even when the
                                // source's zero policy stores positive facts
                                // only and therefore returns no error. The page
                                // is still in the source store, but it selects
                                // no row, so it is not a run member.
                                stats.unobserved += 1;
                            } else {
                                record_outcome(
                                    accumulator,
                                    stats,
                                    previous,
                                    source.source_id,
                                    Some((window.start(), window.end())),
                                    &outcome,
                                );
                            }
                        }
                        Err(AcquireError::Invalid {
                            code: "unverified_zero",
                            ..
                        }) => {
                            // A quiet window is not absence (BR-M3-10): count
                            // it as an unobserved gap for R3. It is still an
                            // observed empty response, not a failed request, so
                            // a full run may complete and advance
                            // verified_through; the gap travels separately.
                            stats.unobserved += 1;
                        }
                        Err(error) => {
                            stats.failed += 1;
                            accumulator.failures.push(RefreshFailure {
                                source_id: source.source_id.as_str().to_string(),
                                window_start: Some(window.start().to_string()),
                                window_end: Some(window.end().to_string()),
                                code: error.code().to_string(),
                                detail: format!("{label}: {error}"),
                            });
                        }
                    }
                }
            }
        }
        PlanClass::Probe => {
            let Some((label, plan)) = source.plans.first() else {
                return Err(RefreshError::Invalid(format!(
                    "{} has no probe plan",
                    source.source_id.as_str()
                )));
            };
            let probe = entry
                .probe_first_page(plan)
                .map_err(table_acquisition_error)?;
            let Some(snapshot) = probe.snapshots.first() else {
                return Err(AcquireError::Invalid {
                    code: "snapshot_incomplete",
                    detail: format!("{label} probe produced no page"),
                }
                .into());
            };
            let changed = probe_changed(
                store,
                plan,
                previous.latest(source.source_id, &snapshot.row.request_fingerprint),
                snapshot,
            )?;
            if !changed {
                // The probe is evidence of change detection, not a selected
                // page: no member, no revision, and the source stays
                // incomplete so the weekly full remains mandatory.
                stats.reused += if snapshot.reused() { 1 } else { 0 };
                return Ok(());
            }
            match acquire_guarded_table(entry, store, plan) {
                Ok(outcome) => {
                    stats.complete = true;
                    record_outcome(
                        accumulator,
                        stats,
                        previous,
                        source.source_id,
                        None,
                        &outcome,
                    );
                }
                Err(error) => {
                    stats.failed += 1;
                    accumulator.failures.push(RefreshFailure {
                        source_id: source.source_id.as_str().to_string(),
                        window_start: None,
                        window_end: None,
                        code: error.code().to_string(),
                        detail: format!("{label} (probe triggered): {error}"),
                    });
                }
            }
        }
    }
    Ok(())
}

fn record_outcome(
    accumulator: &mut Accumulator,
    stats: &mut SourceStats,
    previous: &PreviousSnapshots,
    source: SourceId,
    window: Option<(NaiveDate, NaiveDate)>,
    outcome: &AcquisitionOutcome,
) {
    stats.snapshots += outcome.snapshots.len();
    stats.reused += outcome.reused;
    for snapshot in &outcome.snapshots {
        let row = &snapshot.row;
        let previous_row = previous.latest(source, &row.request_fingerprint);
        let changed = previous_row.is_some_and(|previous| previous.snapshot_id != row.snapshot_id);
        if let Some(previous) = previous_row.filter(|_| changed) {
            stats.changed += 1;
            accumulator.revisions.push(Revision {
                source_id: source.as_str().to_string(),
                request_fingerprint: row.request_fingerprint.clone(),
                previous_snapshot_id: previous.snapshot_id.clone(),
                snapshot_id: row.snapshot_id.clone(),
                previous_as_of: previous.as_of.clone(),
                as_of: row.as_of.clone(),
                row_count: row.row_count,
                page_count: row.page_count,
                window_start: window.map(|(start, _)| start.to_string()),
                window_end: window.map(|(_, end)| end.to_string()),
            });
        }
        accumulator.members.insert(
            (source.as_str().to_string(), row.request_fingerprint.clone()),
            RunMemberRow {
                run_id: String::new(), // filled by the caller's run id below
                source_id: source.as_str().to_string(),
                request_fingerprint: row.request_fingerprint.clone(),
                snapshot_id: row.snapshot_id.clone(),
                page_count: row.page_count,
                row_count: row.row_count,
                as_of: row.as_of.clone(),
                fetched_at: row.fetched_at.clone(),
                changed,
                window_start: window.map(|(start, _)| start.to_string()),
                window_end: window.map(|(_, end)| end.to_string()),
            },
        );
    }
}

/// Acquire a whole table, verify the page set against the frozen bytes, then
/// re-read page 1 and retry once if the table changed mid-fetch (§5.5).
fn acquire_guarded_table(
    entry: &Entry<'_>,
    store: &SourceStore,
    plan: &crate::exchange_source::AcquisitionPlan,
) -> Result<AcquisitionOutcome, AcquireError> {
    for _ in 0..=1 {
        let outcome = entry.acquire(plan).map_err(table_acquisition_error)?;
        verify_table_set(store, plan, &outcome)?;
        let first = outcome
            .snapshots
            .first()
            .ok_or_else(|| snapshot_incomplete("the table produced no page"))?;
        let guard = entry
            .probe_first_page(plan)
            .map_err(table_acquisition_error)?;
        let guard_first = guard
            .snapshots
            .first()
            .ok_or_else(|| snapshot_incomplete("the page-1 re-read produced no page"))?;
        if first.row.snapshot_id == guard_first.row.snapshot_id {
            return Ok(outcome);
        }
        // The first page changed while the later pages were being read: the
        // set is a mix of two table versions. The loop retries once from page
        // 1 and fails after the second mismatch.
    }
    Err(AcquireError::Invalid {
        code: "window_changed_mid_fetch",
        detail: "page 1 changed again after a full retry; the page set is mixed and was not \
                 selected"
            .to_string(),
    })
}

fn verify_table_set(
    store: &SourceStore,
    plan: &crate::exchange_source::AcquisitionPlan,
    outcome: &AcquisitionOutcome,
) -> Result<(), AcquireError> {
    if outcome.page_count as usize != outcome.snapshots.len() {
        return Err(snapshot_incomplete(format!(
            "the table declares {} page(s) but {} page(s) were selected",
            outcome.page_count,
            outcome.snapshots.len()
        )));
    }
    let declared = validate::declared_tabkey(&plan.params);
    let mut rows = 0usize;
    for (index, snapshot) in outcome.snapshots.iter().enumerate() {
        let bytes = store.read_verified(&snapshot.row.snapshot_id)?;
        let envelope = validate::parse_page(plan.format, declared, &bytes).map_err(|error| {
            snapshot_incomplete(format!(
                "selected page {} (snapshot {}) no longer parses: {error}",
                index + 1,
                snapshot.row.snapshot_id
            ))
        })?;
        let expected_page = u32::try_from(index + 1).unwrap_or(u32::MAX);
        if envelope.page != expected_page || envelope.page_count != outcome.page_count {
            return Err(snapshot_incomplete(format!(
                "selected page {} reports page {}/{} but the set expects {}/{}",
                index + 1,
                envelope.page,
                envelope.page_count,
                expected_page,
                outcome.page_count
            )));
        }
        rows += envelope.rows;
    }
    if rows != outcome.row_count {
        return Err(snapshot_incomplete(format!(
            "selected pages hold {rows} row(s) but the acquisition reports {}",
            outcome.row_count
        )));
    }
    Ok(())
}

/// Page-1 change detection for a table probe (D-66 Q3).
///
/// First observation always triggers a full table (`no previous page`). A
/// changed recordcount triggers a full table. For the short-name table, the
/// probe additionally parses the page and triggers only when a **new** row
/// carries `ST`/`*ST`/`退`; an ordinary rename waits for the weekly full.
fn probe_changed(
    store: &SourceStore,
    plan: &crate::exchange_source::AcquisitionPlan,
    previous: Option<&SnapshotRow>,
    current: &FrozenSnapshot,
) -> Result<bool, AcquireError> {
    let Some(previous) = previous else {
        return Ok(true);
    };
    if previous.snapshot_id == current.row.snapshot_id {
        return Ok(false);
    }
    let previous_bytes = store.read_verified(&previous.snapshot_id)?;
    let current_bytes = store.read_verified(&current.row.snapshot_id)?;
    let Some(previous_format) =
        crate::exchange_source::PagingFormat::from_parser_version(&previous.parser_version)
    else {
        // An unreadable old page cannot be compared; the safe action is a full
        // table, not pretending nothing changed.
        return Ok(true);
    };
    // The comparison page belongs to the same request fingerprint, so the
    // current plan's declared TABKEY is the selector the old page was read
    // with. Do not fall back to the short-name table's TABKEY for every SZSE
    // source: a listing page has no TABKEY and would be misread as corrupt.
    let previous_declared = match previous_format {
        crate::exchange_source::PagingFormat::SzseMetadata
        | crate::exchange_source::PagingFormat::SzseMetadataV2 => {
            validate::declared_tabkey(&plan.params).map(str::to_string)
        }
        _ => None,
    };
    let previous_envelope = match validate::parse_page(
        previous_format,
        previous_declared.as_deref(),
        &previous_bytes,
    ) {
        Ok(envelope) => envelope,
        Err(_) => return Ok(true),
    };
    let current_envelope = validate::parse_page(
        plan.format,
        validate::declared_tabkey(&plan.params),
        &current_bytes,
    )
    .map_err(|error| {
        snapshot_incomplete(format!(
            "current probe page {} does not parse: {error}",
            current.row.snapshot_id
        ))
    })?;
    if previous_envelope.total != current_envelope.total {
        return Ok(true);
    }
    if plan.source_id != SourceId::SzseShortName {
        return Ok(false);
    }
    let declared = validate::declared_tabkey(&plan.params);
    let previous_rows = match crate::official_security::risk_warnings::parse_sz_page(
        &previous_bytes,
        &previous.snapshot_id,
        previous_declared.as_deref(),
    ) {
        Ok((rows, _, _)) => rows,
        Err(_) => return Ok(true),
    };
    let current_rows = match crate::official_security::risk_warnings::parse_sz_page(
        &current_bytes,
        &current.row.snapshot_id,
        declared,
    ) {
        Ok((rows, _, _)) => rows,
        Err(_) => return Ok(true),
    };
    let previous_ids = previous_rows
        .iter()
        .map(|row| row.source_record_id.as_str())
        .collect::<BTreeSet<_>>();
    let new_rows: Vec<&SzNameChange> = current_rows
        .iter()
        .filter(|row| !previous_ids.contains(row.source_record_id.as_str()))
        .collect();
    Ok(new_rows
        .iter()
        .any(|row| marked(&row.before_name) || marked(&row.after_name)))
}

fn marked(name: &str) -> bool {
    name.contains("ST") || name.contains('退')
}

fn table_acquisition_error(error: AcquireError) -> AcquireError {
    match error {
        AcquireError::NotAdmitted { .. }
        | AcquireError::Freeze(_)
        | AcquireError::Store(_)
        | AcquireError::Index(_)
        | AcquireError::RefreshOverdue { .. } => error,
        other => snapshot_incomplete(other.to_string()),
    }
}

fn snapshot_incomplete(detail: impl Into<String>) -> AcquireError {
    AcquireError::Invalid {
        code: "snapshot_incomplete",
        detail: detail.into(),
    }
}

/// Run-start view of the latest snapshot per (source, request fingerprint).
struct PreviousSnapshots {
    rows: BTreeMap<(String, String), SnapshotRow>,
}

impl PreviousSnapshots {
    fn load(backend: &MarketBackend) -> Result<Self, RefreshError> {
        let mut rows = BTreeMap::new();
        for row in backend.read_exchange_snapshots()? {
            let key = (row.source_id.clone(), row.request_fingerprint.clone());
            match rows.get(&key) {
                Some(previous)
                    if (previous.fetched_at.as_str(), previous.snapshot_id.as_str())
                        >= (row.fetched_at.as_str(), row.snapshot_id.as_str()) => {}
                _ => {
                    rows.insert(key, row);
                }
            }
        }
        Ok(Self { rows })
    }

    fn latest(&self, source: SourceId, fingerprint: &str) -> Option<&SnapshotRow> {
        self.rows
            .get(&(source.as_str().to_string(), fingerprint.to_string()))
    }
}

#[cfg(test)]
mod tests;
