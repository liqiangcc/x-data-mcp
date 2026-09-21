//! Application dispatch, never an MCP eligibility decision or source fallback.
use crate::{
    data_supply::generation::{BoundSource, GenerationSource, uniform_binding},
    market_storage::Error,
    market_update::runner::{OfficialRangeProvider, RangeProvider, RangeRunner},
    sqlite_market::SqliteMarket,
    sqlite_tasks::SqliteTasks,
    task_control::{TaskRepository, TaskSnapshot, service::TaskControl},
};
/// Resolve this task's bound generation session exactly once per worker
/// turn. Under a configured generation root the frozen
/// `source_generation_id` is the only admissible binding: a uniform
/// Some(id) opens a pinned session, a null/missing/mixed binding keeps the
/// source refusal.
fn source_session(generation: &GenerationSource, candidate: &TaskSnapshot) -> Option<BoundSource> {
    let id = uniform_binding(candidate.items.iter().map(|item| &item.spec)).ok()?;
    generation
        .open_bound(&id, &format!("task:{}", candidate.task.task_id))
        .ok()
}
pub fn run_once(
    control: &TaskControl<SqliteTasks>,
    market: &SqliteMarket,
    market_source: Option<&crate::official_market::TdxMarketSource>,
    generation_source: Option<&GenerationSource>,
) -> Result<Option<TaskSnapshot>, Error> {
    let Some(candidate) = control
        .repository
        .candidates(&control.run_id)?
        .into_iter()
        .next()
    else {
        return Ok(None);
    };
    if let Some(generation) = generation_source {
        generation.sweep(&control.repository);
    }
    let bound = generation_source.and_then(|g| source_session(g, &candidate));
    // In generation mode the bound session's verified source is the only
    // admissible official-file view; without it every source-reading item
    // keeps its refusal. Legacy mode keeps the inspected-env source.
    let market_source = match generation_source {
        Some(_) => bound.as_ref().map(|b| &b.source),
        None => market_source,
    };
    let catalog_executor = bound.as_ref().map(|bound| bound.catalog_executor(market));
    if candidate.task.key.dataset == crate::task_verification::DATASET {
        run_verification(control, market, &candidate)?;
        return Ok(Some(control.read_task(&candidate.task.task_id)?));
    }
    if candidate.task.key.dataset == crate::gap_verification::DATASET {
        let runner = crate::gap_verification::runner::GapVerifyRunner {
            repository: market.clone(),
            run_id: control.run_id.clone(),
        };
        let now = chrono::Utc::now();
        match market_source {
            // An installed inspected snapshot verifies real-source gaps;
            // without it the unverified port stays fail-closed.
            Some(source) => runner.run(
                &candidate.task.task_id,
                &crate::official_market::TdxGapVerifier {
                    source,
                    repository: market.clone(),
                    observed_at: now,
                },
                now,
            )?,
            None => runner.run(
                &candidate.task.task_id,
                &crate::gap_verification::UnverifiedGapVerifier,
                now,
            )?,
        };
        return Ok(Some(control.read_task(&candidate.task.task_id)?));
    }
    if candidate.task.key.dataset == "securities.catalog" {
        // The catalog item executes against a generation-bound session when
        // the task froze one; otherwise the legacy inspected-env location;
        // absent configuration retains the source refusal.
        let executor = match catalog_executor {
            Some(executor) => Some(executor),
            None if generation_source.is_none() => {
                crate::catalog_tdx::source::TdxCatalogExecutor::from_env(market)
            }
            None => None,
        };
        return match executor {
            Some(executor) => Ok(control.run_selected(candidate, &executor)?),
            None => Ok(control.run_selected(candidate, &crate::official_files::BlockedSource)?),
        };
    }
    if !matches!(
        candidate.task.key.operation.as_str(),
        "update_data" | "backfill_data" | "republish_data"
    ) {
        // Only the market.daily collection item may run against an installed
        // inspected-snapshot source; catalog/yearly/legacy items and any
        // service without a configured snapshot keep the source refusal.
        if candidate.task.key.operation == "start_collection"
            && candidate.task.key.dataset == "market.daily"
            && candidate.task.key.contract_version == "v1"
            && let Some(source) = market_source
        {
            // One session per worker run: the gbbq member decode is shared
            // across every item in this task; single-item callers keep the
            // plain path through the runner's `session: None` default.
            let session = source.materialize_session();
            let runner = crate::official_market::TdxDailyRunner {
                source,
                session: Some(&session),
                repository: market.clone(),
                run_id: control.run_id.clone(),
                published_at: chrono::Utc::now(),
            };
            for item in &candidate.items {
                runner.run_item(&candidate.task.task_id, &item.spec.item_id)?;
            }
            return Ok(Some(control.read_task(&candidate.task.task_id)?));
        }
        return Ok(control.run_selected(candidate, &crate::official_files::BlockedSource)?);
    }
    if candidate.task.key.dataset == "market.yearly" {
        let runner = crate::yearly_publication::runner::YearlyRunner {
            repository: market.clone(),
            run_id: control.run_id.clone(),
            derived_at: chrono::Utc::now(),
        };
        for item in &candidate.items {
            runner.run_item(
                &candidate.task.task_id,
                &item.spec.item_id,
                &crate::yearly_publication::runner::OfficialYearlyVerifier,
            )?;
        }
        return Ok(Some(control.read_task(&candidate.task.task_id)?));
    }
    let runner = RangeRunner {
        repository: market.clone(),
        run_id: control.run_id.clone(),
    };
    let update = market_source.map(|source| crate::official_market::TdxRangeProvider { source });
    let backfill = market_source.map(|source| crate::official_market::TdxBackfillProvider {
        source,
        published_at: chrono::Utc::now(),
    });
    let republish = market_source.map(|source| crate::official_market::TdxRepublishProvider {
        source,
        published_at: chrono::Utc::now(),
    });
    for item in &candidate.items {
        // `update_data` refresh uses the range provider; `backfill_data`
        // gains a source path only under the default frozen-gap contract;
        // `republish_data` gains one only under the explicit republish
        // contract (#247) — everything else keeps the blocked provider.
        let provider: &dyn RangeProvider = match (
            candidate.task.key.operation.as_str(),
            candidate.task.key.contract_version.as_str(),
            update.as_ref(),
            backfill.as_ref(),
            republish.as_ref(),
        ) {
            ("update_data", _, Some(provider), _, _) => provider,
            ("backfill_data", contract, _, Some(provider), _)
                if contract == crate::market_update::tasks::DEFAULT_CONTRACT_VERSION =>
            {
                provider
            }
            ("republish_data", contract, _, _, Some(provider))
                if contract == crate::market_update::tasks::REPUBLISH_CONTRACT_VERSION =>
            {
                provider
            }
            _ => &OfficialRangeProvider,
        };
        runner.run_item(&candidate.task.task_id, &item.spec.item_id, provider)?;
    }
    Ok(Some(control.read_task(&candidate.task.task_id)?))
}

#[cfg(target_os = "linux")]
fn run_verification(
    control: &TaskControl<SqliteTasks>,
    market: &SqliteMarket,
    candidate: &TaskSnapshot,
) -> Result<(), Error> {
    if !crate::verification_execution::supported(&candidate.task.key) {
        return Err(crate::task_control::Error::UnsupportedContract.into());
    }
    // Owned by the runner, so Claim, evidence I/O, receipt reconciliation and
    // cancellation/error handling all finish before the kernel guard is dropped.
    let tasks = crate::sqlite_verification_runtime::guarded::GuardedTasks::open(
        control.repository.clone(),
        &candidate.task.task_id,
        &control.run_id,
    )?;
    if candidate.task.key == crate::publication_verification::admission::command_key() {
        crate::publication_verification::runner::Runner {
            tasks,
            evidence: market.clone(),
            run_id: control.run_id.clone(),
            actor: "local-owner".into(),
        }
        .run(&candidate.task.task_id)?;
    } else {
        crate::task_verification::runner::StopRunner {
            repository: tasks,
            run_id: control.run_id.clone(),
            actor: "local-owner".into(),
        }
        .run(
            &candidate.task.task_id,
            &crate::task_verification::determinate::Audited {
                repository: control.repository.clone(),
            },
        )?;
    }
    Ok(())
}
#[cfg(not(target_os = "linux"))]
fn run_verification(
    _: &TaskControl<SqliteTasks>,
    _: &SqliteMarket,
    _: &TaskSnapshot,
) -> Result<(), Error> {
    Err(crate::task_control::Error::NeedsVerification(
        "verification runtime exclusion is unsupported on this platform".into(),
    )
    .into())
}
