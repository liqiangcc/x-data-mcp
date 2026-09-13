//! Pure eligibility and disposition. Runtime observations are not wire input.
use super::{Conclusion, Receipt, TerminalResult};
use crate::{
    sqlite_verification_runtime::Registration,
    task_control::{Command, Error, Stage, Status, TaskSnapshot},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Facts {
    pub(crate) target: TaskSnapshot,
    pub(crate) original: Option<TaskSnapshot>,
    pub(crate) registration: Option<Registration>,
    pub(crate) finished: bool,
    pub(crate) has_business_effects: bool,
    pub(crate) completion_digest: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Observation {
    NotNeeded,
    Excluded,
    Busy,
    Unverified(String),
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Audit {
    pub request_id: String,
    pub command: Command,
    pub actor: String,
    pub run_id: String,
    pub checked_at: DateTime<Utc>,
    pub facts: Facts,
    pub observation: Observation,
    pub after: Option<TaskSnapshot>,
    pub result: TerminalResult,
}
pub struct Plan {
    pub(crate) audit: Audit,
}
impl Plan {
    pub fn result(&self) -> &TerminalResult {
        &self.audit.result
    }
}
fn never_started(f: &Facts) -> bool {
    f.registration.is_none()
        && f.target.task.owner_run_id.is_none()
        && f.target.task.state_revision == 0
        && f.target.task.status == Status::Pending
        && f.target.task.result.is_none()
        && f.target
            .items
            .iter()
            .all(|i| i.stage == Stage::Pending && i.outcome.is_none())
}
pub fn needs_probe(f: &Facts) -> bool {
    crate::verification_execution::supported(&f.target.task.key)
        && !f.finished
        && f.registration.is_some()
}
pub fn prepare(
    id: &str,
    command: Command,
    facts: Facts,
    observation: Observation,
    run: &str,
    actor: &str,
    now: DateTime<Utc>,
) -> Result<Plan, Error> {
    if id.trim().is_empty()
        || run.trim().is_empty()
        || actor.trim().is_empty()
        || command != super::command(&facts.target.task.task_id)?
    {
        return Err(Error::InvalidCommand(
            "inspection identity/command missing or inconsistent".into(),
        ));
    }
    validate_facts(&facts)?;
    let target = &facts.target;
    let supported = crate::verification_execution::supported(&target.task.key);
    let old = target.visible_status(run) == Status::NeedsVerification;
    let (conclusion, reason) = if !supported {
        (
            Conclusion::NotApplicable,
            "target is not a supported verification task".into(),
        )
    } else if facts.finished {
        (
            Conclusion::AlreadyFinished,
            "verified persisted finish; existing result is not rewritten".into(),
        )
    } else if facts.has_business_effects {
        (
            Conclusion::InsufficientEvidence,
            "verification task has unexpected business effects; protection retained".into(),
        )
    } else if matches!(observation, Observation::Excluded) && facts.registration.is_some() {
        (Conclusion::Interrupted, "registered execution excluded and verified transaction history has no committed finish; close only verification task".into())
    } else if old && never_started(&facts) {
        (Conclusion::Interrupted, "verified never-claimed execution with no effects; conditional commit excludes future Claim".into())
    } else if !old && matches!(observation, Observation::Busy | Observation::NotNeeded) {
        (
            Conclusion::NotApplicable,
            "current execution has no applicable abnormality; no forced interruption".into(),
        )
    } else {
        (Conclusion::InsufficientEvidence, match &observation {
            Observation::Busy => "registered execution is still active; no stopping proof".into(),
            Observation::Unverified(reason) => reason.clone(),
            _ => "no registered execution exclusion proof; old run or missing receipt does not prove stopped".into(),
        })
    };
    let mut after = None;
    if conclusion == Conclusion::Interrupted {
        let mut next = target.clone();
        next.task.status = Status::Interrupted;
        for item in &mut next.items {
            if item.stage == Stage::Running {
                item.stage = Stage::Pending;
            }
        }
        next.task.result = Some(
            json!({"interruption":{"inspection_request_id":id},"previous_result":target.task.result,"unfinished_items":next.items.iter().filter(|i| !i.outcome.as_ref().is_some_and(|o|o.success)).map(|i|&i.spec.item_id).collect::<Vec<_>>()}),
        );
        next.claims.clear();
        after = Some(next);
    } else if conclusion == Conclusion::InsufficientEvidence
        && target.task.status != Status::NeedsVerification
    {
        let mut next = target.clone();
        next.task.status = Status::NeedsVerification;
        after = Some(next);
    }
    if let Some(next) = &mut after {
        next.task.state_revision = next
            .task
            .state_revision
            .checked_add(1)
            .ok_or(Error::RevisionConflict)?;
        next.task.last_progress_at = now;
        for claim in &mut next.claims {
            claim.state_revision = next.task.state_revision;
        }
    }
    let actual = after.as_ref().unwrap_or(target);
    let status = serde_json::to_value(actual.task.status)
        .map_err(|e| Error::NeedsVerification(e.to_string()))?;
    let result = TerminalResult::VerificationInspection {
        receipt: Receipt {
            request_id: id.into(),
            target_task_id: target.task.task_id.clone(),
            actor: actor.into(),
            checked_at: now,
            conclusion,
            target_revision: actual.task.state_revision,
            target_status: status.as_str().unwrap_or("unknown").into(),
            target_result: actual.task.result.clone(),
            reason,
        },
    };
    Ok(Plan {
        audit: Audit {
            request_id: id.into(),
            command,
            actor: actor.into(),
            run_id: run.into(),
            checked_at: now,
            facts,
            observation,
            after,
            result,
        },
    })
}
pub(crate) fn validate(a: &Audit) -> Result<(), Error> {
    let expected = prepare(
        &a.request_id,
        a.command.clone(),
        a.facts.clone(),
        a.observation.clone(),
        &a.run_id,
        &a.actor,
        a.checked_at,
    )?;
    if expected.audit != *a {
        return Err(Error::NeedsVerification(
            "inspection audit projections differ".into(),
        ));
    }
    Ok(())
}

fn validate_facts(f: &Facts) -> Result<(), Error> {
    if !crate::verification_execution::supported(&f.target.task.key) {
        return Ok(());
    }
    let original = f
        .original
        .as_ref()
        .ok_or_else(|| Error::NeedsVerification("inspection original target absent".into()))?;
    let scope = crate::task_verification::verification_scope(&original.task.task_id);
    let expected_scopes = [scope]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    let expected_item = if f.target.task.key == crate::task_verification::command_key() {
        "stopped-writes"
    } else {
        "publication-results"
    };
    if f.target.items.len() != 1
        || f.target.task.work.scopes != expected_scopes
        || f.target.items[0].spec.scope_keys != expected_scopes
        || f.target.items[0].spec.item_id != expected_item
        || f.target.items[0].spec.progress_unit != "evidence_check"
        || f.target.items[0].spec.target != json!({"task_id":original.task.task_id})
        || f.target.items[0].spec.frozen_inputs["target_task_id"] != json!(original.task.task_id)
        || f.finished != f.target.task.status.terminal()
        || (!f.finished && f.completion_digest.is_some())
    {
        return Err(Error::NeedsVerification(
            "inspection target/work/serial protection association differs".into(),
        ));
    }
    Ok(())
}
