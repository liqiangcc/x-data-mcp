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
        if candidate.task.key == crate::publication_verification::admission::command_key() {
            crate::publication_verification::runner::Runner {
                tasks: control.repository.clone(),
                evidence: market.clone(),
                run_id: control.run_id.clone(),
                actor: "local-owner".into(),
            }
            .run(&candidate.task.task_id)?;
        } else if candidate.task.key == crate::task_verification::command_key() {
            crate::task_verification::runner::StopRunner {
                repository: control.repository.clone(),
                run_id: control.run_id.clone(),
                actor: "local-owner".into(),
            }
            .run(
                &candidate.task.task_id,
                &crate::task_verification::never_started::NeverStarted {
                    repository: control.repository.clone(),
                },
            )?;
        } else {
            return Err(crate::task_control::Error::UnsupportedContract.into());
        }
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
