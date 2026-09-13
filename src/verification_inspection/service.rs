//! Bounded application orchestration. A replay does not probe current execution.
use super::decision::{Facts, Observation, Plan, needs_probe, prepare};
use crate::{
    task_control::{Acceptance, Error, TaskRepository},
    verification_runtime::ExecutionGuard,
};
pub struct Probe {
    pub(crate) observation: Observation,
    pub(crate) guard: Option<ExecutionGuard>,
}
pub trait Repository: TaskRepository {
    fn inspection_facts(&self, id: &str) -> Result<Facts, Error>;
    fn probe_execution(&self, facts: &Facts) -> Result<Probe, Error>;
    fn commit_inspection(&self, plan: &Plan, probe: &Probe) -> Result<Acceptance, Error>;
}
pub fn inspect<R: Repository>(
    repository: &R,
    request: &str,
    target: &str,
    run: &str,
    actor: &str,
) -> Result<Acceptance, Error> {
    if request.trim().is_empty() {
        return Err(Error::InvalidCommand("empty request_id".into()));
    }
    let command = super::command(target)?;
    if let Some(record) = repository.read_request(request)? {
        if record.command != command {
            return Err(Error::RequestConflict);
        }
        return Ok(Acceptance {
            record,
            reused: true,
        });
    }
    let facts = repository.inspection_facts(target)?;
    let probe = if needs_probe(&facts) {
        repository.probe_execution(&facts)?
    } else {
        Probe {
            observation: Observation::NotNeeded,
            guard: None,
        }
    };
    let plan = prepare(
        request,
        command,
        facts,
        probe.observation.clone(),
        run,
        actor,
        chrono::Utc::now(),
    )?;
    match repository.commit_inspection(&plan, &probe) {
        Ok(result) => Ok(result),
        Err(error) => {
            // One read reconciles a lost response; no retry, wait or stale reapply.
            if let Some(record) = repository.read_request(request)? {
                if record.command != plan.audit.command {
                    return Err(Error::RequestConflict);
                }
                return Ok(Acceptance {
                    record,
                    reused: true,
                });
            }
            Err(error)
        }
    }
}
