use super::DATASET;
use crate::task_control::{
    CommandKey, Contract, Error, FrozenWork, ItemSpec, Status, TaskRepository,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    target_task_id: String,
}
fn invalid(error: impl ToString) -> Error {
    Error::InvalidCommand(error.to_string())
}
/// All verification types of the same task share this resource. It is distinct
/// from every original business resource; taking it cannot release those claims.
pub fn verification_scope(task_id: &str) -> String {
    format!("verification:task:{task_id}")
}
pub struct StopVerification<R> {
    repository: R,
    run_id: String,
}
impl<R: TaskRepository> StopVerification<R> {
    pub fn new(repository: R, run_id: String) -> Result<Self, Error> {
        if run_id.trim().is_empty() {
            return Err(invalid("empty verification run"));
        }
        Ok(Self { repository, run_id })
    }
}
impl<R: TaskRepository> Contract for StopVerification<R> {
    fn key(&self) -> CommandKey {
        super::command_key()
    }
    fn normalize(&self, payload: Value) -> Result<Value, Error> {
        let input: Input = serde_json::from_value(payload).map_err(invalid)?;
        if input.target_task_id.trim().is_empty() {
            return Err(invalid("empty target_task_id"));
        }
        serde_json::to_value(input).map_err(invalid)
    }
    fn resolve(&self, intent: &Value) -> Result<FrozenWork, Error> {
        let input: Input = serde_json::from_value(intent.clone()).map_err(invalid)?;
        let target = self
            .repository
            .read_task(&input.target_task_id)?
            .ok_or(Error::TaskNotFound)?;
        if target.task.key.dataset == DATASET {
            return Err(invalid(
                "verification tasks require the separate bounded exception inspection",
            ));
        }
        if target.visible_status(&self.run_id) != Status::NeedsVerification {
            return Err(invalid(
                "verification_not_applicable: target is not awaiting verification",
            ));
        }
        let scopes = [verification_scope(&input.target_task_id)]
            .into_iter()
            .collect();
        let item = ItemSpec {
            item_id: "stopped-writes".into(),
            target: json!({"task_id":input.target_task_id}),
            progress_unit: "evidence_check".into(),
            frozen_inputs: json!({
                "target_task_id":input.target_task_id,
                "admission_revision":target.task.state_revision,
                "admission_snapshot_sha256":crate::hash::sha256(&serde_json::to_vec(&target).map_err(invalid)?),
                "accepted_run_id":target.task.accepted_run_id,
                "owner_run_id":target.task.owner_run_id
            }),
            scope_keys: scopes,
        };
        Ok(FrozenWork {
            scopes: item.scope_keys.clone(),
            items: vec![item],
        })
    }
}
