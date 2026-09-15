//! Application dispatch, never an MCP eligibility decision or source fallback.
use crate::{
    market_storage::Error,
    market_update::runner::{OfficialRangeProvider, RangeRunner},
    sqlite_market::SqliteMarket,
    sqlite_tasks::SqliteTasks,
    task_control::{TaskRepository, TaskSnapshot, service::TaskControl},
};
pub fn run_once(
    control: &TaskControl<SqliteTasks>,
    market: &SqliteMarket,
) -> Result<Option<TaskSnapshot>, Error> {
    let Some(candidate) = control
        .repository
        .candidates(&control.run_id)?
        .into_iter()
        .next()
    else {
        return Ok(None);
    };
    if candidate.task.key.dataset == crate::task_verification::DATASET {
        run_verification(control, market, &candidate)?;
        return Ok(Some(control.read_task(&candidate.task.task_id)?));
    }
    if candidate.task.key.dataset == crate::gap_verification::DATASET {
        let runner = crate::gap_verification::runner::GapVerifyRunner {
            repository: market.clone(),
            run_id: control.run_id.clone(),
        };
        runner.run(
            &candidate.task.task_id,
            &crate::gap_verification::UnverifiedGapVerifier,
            chrono::Utc::now(),
        )?;
        return Ok(Some(control.read_task(&candidate.task.task_id)?));
    }
    if !matches!(
        candidate.task.key.operation.as_str(),
        "update_data" | "backfill_data"
    ) {
        // Compatibility collection tasks retain their existing source refusal.
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
    for item in &candidate.items {
        runner.run_item(
            &candidate.task.task_id,
            &item.spec.item_id,
            &OfficialRangeProvider,
        )?;
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
            &crate::task_verification::never_started::NeverStarted {
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
