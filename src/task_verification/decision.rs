//! Decisions are made from trusted inspection output, never MCP evidence fields.
use super::DATASET;
use crate::task_control::{Error, ItemOutcome, Stage, Status, TaskSnapshot};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Output of an installed trusted evidence provider. This is deliberately NOT
/// Deserialize: a JSON claim is not an implementation of the inspection port.
#[derive(Debug, Clone, Serialize)]
pub enum Observation {
    Insufficient {
        reason: String,
    },
    Stopped {
        snapshot_sha256: String,
        evidence_ref: String,
    },
}
/// Implementations must prove all writers stopped and all durable effects have
/// determinate outcomes. PID absence or run identity alone cannot prove either.
pub trait StopInspector {
    fn inspect(&self, target: &TaskSnapshot) -> Result<Observation, Error>;
}
pub struct UnverifiedStopInspector;
impl StopInspector for UnverifiedStopInspector {
    fn inspect(&self, _: &TaskSnapshot) -> Result<Observation, Error> {
        Ok(Observation::Insufficient {
            reason: "STOP_EVIDENCE_NOT_VERIFIED: no installed writer/effect evidence protocol"
                .into(),
        })
    }
}
pub fn snapshot_digest(target: &TaskSnapshot) -> Result<String, Error> {
    Ok(crate::hash::sha256(
        &serde_json::to_vec(target).map_err(|e| Error::NeedsVerification(e.to_string()))?,
    ))
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Conclusion {
    Interrupted,
    InsufficientEvidence,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub verification_task_id: String,
    pub target_task_id: String,
    pub conclusion: Conclusion,
    pub verification_revision: u64,
    pub target_revision: u64,
    pub reason: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Audit {
    pub actor: String,
    pub checked_at: DateTime<Utc>,
    pub target_before: TaskSnapshot,
    pub target_after: Option<TaskSnapshot>,
    pub verification_before: TaskSnapshot,
    pub verification_after: TaskSnapshot,
    pub receipt: Receipt,
    pub evidence_ref: Option<String>,
}
/// Sealed application decision; adapters may apply it, not construct approval.
pub struct StopPlan {
    pub(crate) audit: Audit,
}
impl StopPlan {
    pub fn receipt(&self) -> &Receipt {
        &self.audit.receipt
    }
}
fn changed(s: &mut TaskSnapshot, now: DateTime<Utc>) -> Result<(), Error> {
    s.task.state_revision = s
        .task
        .state_revision
        .checked_add(1)
        .ok_or(Error::RevisionConflict)?;
    s.task.last_progress_at = now;
    s.claims.clear();
    Ok(())
}
pub fn prepare(
    verification: TaskSnapshot,
    target: TaskSnapshot,
    run: &str,
    actor: &str,
    now: DateTime<Utc>,
    inspector: &dyn StopInspector,
) -> Result<StopPlan, Error> {
    if actor.trim().is_empty() || run.trim().is_empty() {
        return Err(Error::InvalidCommand(
            "verification actor/run missing".into(),
        ));
    }
    if verification.task.key.dataset != DATASET
        || verification.task.key.operation != "verify_stopped_writes"
        || verification.task.owner_run_id.as_deref() != Some(run)
        || verification.visible_status(run) != Status::Running
        || verification.items.len() != 1
        || verification.items[0].stage != Stage::Running
        || verification.items[0].spec.frozen_inputs["target_task_id"] != target.task.task_id
    {
        return Err(Error::RevisionConflict);
    }
    if target.task.key.dataset == DATASET || target.visible_status(run) != Status::NeedsVerification
    {
        return Err(Error::RevisionConflict);
    }
    let observation = inspector.inspect(&target)?;
    let (after, conclusion, reason, evidence_ref) = match observation {
        Observation::Insufficient { reason } => {
            if reason.trim().is_empty() {
                return Err(Error::InvalidCommand("missing insufficiency reason".into()));
            }
            (None, Conclusion::InsufficientEvidence, reason, None)
        }
        Observation::Stopped {
            snapshot_sha256,
            evidence_ref,
        } => {
            if snapshot_sha256 != snapshot_digest(&target)? || evidence_ref.trim().is_empty() {
                return Err(Error::NeedsVerification(
                    "stop evidence does not bind inspected target".into(),
                ));
            }
            let mut next = target.clone();
            next.task.status = Status::Interrupted;
            // Preserve successful/failed item facts. A previously running item
            // is now unfinished, with its last known stage retained in audit.
            for item in &mut next.items {
                if item.stage == Stage::Running {
                    item.stage = Stage::Pending;
                }
            }
            next.task.result = Some(
                json!({"interruption":{"verification_task_id":verification.task.task_id,"evidence_ref":evidence_ref},"unfinished_items":next.items.iter().filter(|i|!i.outcome.as_ref().is_some_and(|o|o.success)).map(|i|i.spec.item_id.clone()).collect::<Vec<_>>()}),
            );
            changed(&mut next, now)?;
            (
                Some(next),
                Conclusion::Interrupted,
                "verified stopped; original task not resumed".into(),
                Some(evidence_ref),
            )
        }
    };
    let mut finished = verification.clone();
    finished.task.status = Status::Succeeded;
    finished.items[0].stage = Stage::Finished;
    finished.items[0].outcome = Some(ItemOutcome {
        success: true,
        error: None,
    });
    changed(&mut finished, now)?;
    let receipt = Receipt {
        verification_task_id: verification.task.task_id.clone(),
        target_task_id: target.task.task_id.clone(),
        conclusion,
        verification_revision: finished.task.state_revision,
        target_revision: after.as_ref().unwrap_or(&target).task.state_revision,
        reason,
    };
    // A successful CHECK with insufficient evidence does not make the target
    // successful. Its explicit conclusion is persisted and queried separately.
    finished.task.result = Some(json!({"stopped_write_check":receipt}));
    Ok(StopPlan {
        audit: Audit {
            actor: actor.into(),
            checked_at: now,
            target_before: target,
            target_after: after,
            verification_before: verification,
            verification_after: finished,
            receipt,
            evidence_ref,
        },
    })
}

// Reconstruct the recorded decision to detect inconsistent stored projections.
// This validates a historical audit, not fresh external stopping evidence.
pub(crate) fn validate_audit(audit: &Audit) -> Result<(), Error> {
    struct Recorded<'a>(&'a Audit);
    impl StopInspector for Recorded<'_> {
        fn inspect(&self, target: &TaskSnapshot) -> Result<Observation, Error> {
            Ok(match &self.0.evidence_ref {
                Some(evidence_ref) => Observation::Stopped {
                    snapshot_sha256: snapshot_digest(target)?,
                    evidence_ref: evidence_ref.clone(),
                },
                None => Observation::Insufficient {
                    reason: self.0.receipt.reason.clone(),
                },
            })
        }
    }
    let run = audit
        .verification_before
        .task
        .owner_run_id
        .as_deref()
        .unwrap_or("");
    let expected = prepare(
        audit.verification_before.clone(),
        audit.target_before.clone(),
        run,
        &audit.actor,
        audit.checked_at,
        &Recorded(audit),
    )?;
    if &expected.audit != audit {
        return Err(Error::NeedsVerification(
            "stopped check audit projections differ".into(),
        ));
    }
    Ok(())
}
pub trait StopRepository {
    fn commit_stop_check(&self, plan: &StopPlan) -> Result<Receipt, Error>;
    fn stop_receipt(&self, verification_task_id: &str) -> Result<Option<Receipt>, Error>;
}
