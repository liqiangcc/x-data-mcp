//! Admission checks task eligibility; heavy publication inspection runs later.
use crate::task_control::{
    CommandKey, Contract, Error, FrozenWork, ItemSpec, Stage, Status, TaskRepository,
};
use serde::Deserialize;
use serde_json::{Value, json};
pub const CONTRACT_VERSION: &str = "publication-check-task-v1";
pub fn command_key() -> CommandKey {
    CommandKey {
        operation: "verify_publication_result".into(),
        dataset: crate::task_verification::DATASET.into(),
        contract_version: CONTRACT_VERSION.into(),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    target_task_id: String,
}
pub struct PublicationVerification<R> {
    repository: R,
    run_id: String,
}
impl<R: TaskRepository> PublicationVerification<R> {
    pub fn new(repository: R, run_id: String) -> Result<Self, Error> {
        if run_id.trim().is_empty() {
            return Err(Error::InvalidCommand("empty verification run".into()));
        }
        Ok(Self { repository, run_id })
    }
}
impl<R: TaskRepository> Contract for PublicationVerification<R> {
    fn key(&self) -> CommandKey {
        command_key()
    }
    fn normalize(&self, input: Value) -> Result<Value, Error> {
        let input: Input =
            serde_json::from_value(input).map_err(|e| Error::InvalidCommand(e.to_string()))?;
        if input.target_task_id.trim().is_empty() {
            return Err(Error::InvalidCommand("empty target_task_id".into()));
        }
        Ok(json!({"target_task_id":input.target_task_id}))
    }
    fn resolve(&self, input: &Value) -> Result<FrozenWork, Error> {
        let input: Input = serde_json::from_value(input.clone())
            .map_err(|e| Error::InvalidCommand(e.to_string()))?;
        let target = self
            .repository
            .read_task(&input.target_task_id)?
            .ok_or(Error::TaskNotFound)?;
        if super::contract_kind(&target.task.key).is_none() {
            return Err(Error::UnsupportedContract);
        }
        if target.visible_status(&self.run_id) != Status::NeedsVerification
            || !target.items.iter().any(|i| i.stage == Stage::Running)
        {
            return Err(Error::InvalidCommand("verification_not_applicable: no unresolved execution requiring publication inspection".into()));
        }
        // Verified reads validate any persistent uncertainty marker. An old run
        // can also have crashed before recording it; that is not proof of failure.
        target.execution_uncertainty()?;
        let scopes = [crate::task_verification::verification_scope(
            &input.target_task_id,
        )]
        .into_iter()
        .collect();
        let item = ItemSpec {
            item_id: "publication-results".into(),
            target: json!({"task_id":input.target_task_id}),
            progress_unit: "evidence_check".into(),
            frozen_inputs: json!({"target_task_id":input.target_task_id,"admission_revision":target.task.state_revision,"accepted_run_id":target.task.accepted_run_id,"admission_snapshot_sha256":crate::task_verification::decision::snapshot_digest(&target)?}),
            scope_keys: scopes,
        };
        Ok(FrozenWork {
            scopes: item.scope_keys.clone(),
            items: vec![item],
        })
    }
}
