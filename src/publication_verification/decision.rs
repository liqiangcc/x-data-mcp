//! Publication conclusions do not decide whether original writers have stopped.
use super::{Evidence, Facts, admission::command_key};
use crate::task_control::{Error, ItemOutcome, Stage, Status, TaskSnapshot};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Conclusion {
    AllRecorded,
    InsufficientEvidence,
    NotApplicable,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Receipt {
    pub verification_task_id: String,
    pub target_task_id: String,
    pub verification_revision: u64,
    pub target_revision: u64,
    pub conclusion: Conclusion,
    pub reason: String,
    pub missing_receipt_item_ids: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Audit {
    pub actor: String,
    pub checked_at: DateTime<Utc>,
    pub facts: Facts,
    pub verification_before: TaskSnapshot,
    pub verification_after: TaskSnapshot,
    pub receipt: Receipt,
}
pub struct Plan {
    pub(crate) audit: Audit,
}
impl Plan {
    pub fn receipt(&self) -> &Receipt {
        &self.audit.receipt
    }
    pub fn finished_check(&self) -> &TaskSnapshot {
        &self.audit.verification_after
    }
}
pub fn prepare(
    check: TaskSnapshot,
    facts: Facts,
    run: &str,
    actor: &str,
    now: DateTime<Utc>,
) -> Result<Plan, Error> {
    let target = &facts.task;
    let scope = [crate::task_verification::verification_scope(
        &target.task.task_id,
    )]
    .into_iter()
    .collect();
    if run.trim().is_empty()
        || actor.trim().is_empty()
        || check.task.key != command_key()
        || check.visible_status(run) != Status::Running
        || check.task.owner_run_id.as_deref() != Some(run)
        || check.task.work.scopes != scope
        || check.items.len() != 1
        || check.items[0].stage != Stage::Running
        || check.items[0].spec.item_id != "publication-results"
        || check.items[0].spec.progress_unit != "evidence_check"
        || check.items[0].spec.scope_keys != scope
        || check.items[0].spec.frozen_inputs["target_task_id"] != target.task.task_id
        || check.items[0].spec.frozen_inputs["accepted_run_id"] != target.task.accepted_run_id
        || super::contract_kind(&target.task.key).is_none()
    {
        return Err(Error::RevisionConflict);
    }
    if facts.items.len() != target.items.len() || facts.items.is_empty() {
        return Err(Error::NeedsVerification(
            "incomplete publication evidence item set".into(),
        ));
    }
    let mut ids = std::collections::BTreeSet::new();
    let mut all = true;
    for fact in &facts.items {
        let item = target
            .items
            .iter()
            .find(|i| i.spec.item_id == fact.item_id)
            .ok_or_else(|| Error::NeedsVerification("publication fact has unknown item".into()))?;
        if !ids.insert(&fact.item_id) {
            return Err(Error::NeedsVerification(
                "duplicate publication evidence item".into(),
            ));
        }
        match &fact.evidence {
            Evidence::Missing => {
                if item.outcome.as_ref().is_some_and(|o| o.success) {
                    return Err(Error::NeedsVerification(
                        "successful item lacks publication evidence".into(),
                    ));
                }
                all = false;
            }
            Evidence::Committed {
                receipt,
                evidence_sha256,
                ..
            } => {
                if item.stage != Stage::Finished
                    || !item.outcome.as_ref().is_some_and(|o| o.success)
                    || receipt.task_id != target.task.task_id
                    || receipt.task_revision > target.task.state_revision
                    || evidence_sha256.len() != 64
                    || !evidence_sha256.bytes().all(|b| b.is_ascii_hexdigit())
                {
                    return Err(Error::NeedsVerification(
                        "publication evidence does not bind successful item".into(),
                    ));
                }
            }
        }
    }
    let conclusion = if all {
        Conclusion::AllRecorded
    } else if target.visible_status(run) == Status::NeedsVerification {
        Conclusion::InsufficientEvidence
    } else {
        Conclusion::NotApplicable
    };
    let missing_receipt_item_ids = facts
        .items
        .iter()
        .filter(|f| matches!(f.evidence, Evidence::Missing))
        .map(|f| f.item_id.clone())
        .collect();
    let reason = match conclusion {
        Conclusion::AllRecorded => "all task items have verified fixed publication evidence; this does not prove writers stopped",
        Conclusion::InsufficientEvidence => "one or more task items lack a committed publication receipt; absence does not prove failure or non-publication; original protection retained",
        Conclusion::NotApplicable => "target no longer requires publication inspection; missing receipts are not classified as publication failure",
    }.into();
    let mut after = check.clone();
    after.task.status = Status::Succeeded;
    after.task.state_revision = after
        .task
        .state_revision
        .checked_add(1)
        .ok_or(Error::RevisionConflict)?;
    after.task.last_progress_at = now;
    after.items[0].stage = Stage::Finished;
    after.items[0].outcome = Some(ItemOutcome {
        success: true,
        error: None,
        evidence: None,
    });
    after.claims.clear();
    let receipt = Receipt {
        verification_task_id: check.task.task_id.clone(),
        target_task_id: target.task.task_id.clone(),
        verification_revision: after.task.state_revision,
        target_revision: target.task.state_revision,
        conclusion,
        reason,
        missing_receipt_item_ids,
    };
    after.task.result = Some(json!({"publication_check":receipt}));
    Ok(Plan {
        audit: Audit {
            actor: actor.into(),
            checked_at: now,
            facts,
            verification_before: check,
            verification_after: after,
            receipt,
        },
    })
}

pub(crate) fn validate_audit(audit: &Audit) -> Result<(), Error> {
    let run = audit
        .verification_before
        .task
        .owner_run_id
        .as_deref()
        .unwrap_or("");
    let expected = prepare(
        audit.verification_before.clone(),
        audit.facts.clone(),
        run,
        &audit.actor,
        audit.checked_at,
    )?;
    if expected.audit != *audit {
        return Err(Error::NeedsVerification(
            "publication check audit projections differ".into(),
        ));
    }
    Ok(())
}
pub trait AuditStore {
    fn commit_check(&self, plan: &Plan) -> Result<Receipt, crate::market_storage::Error>;
    fn check_receipt(&self, id: &str) -> Result<Option<Receipt>, crate::market_storage::Error>;
}
