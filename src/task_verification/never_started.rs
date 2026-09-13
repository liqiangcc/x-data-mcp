//! Proof for audited executors that must claim before performing any work.
//! This does not establish stopping of an executor which has already started.
use super::decision::{Observation, StopInspector, snapshot_digest};
use crate::task_control::{Contract, Error, Stage, Status, TaskSnapshot, legacy::LegacyCollection};

pub struct Facts {
    pub snapshot: TaskSnapshot,
    pub has_durable_effects: bool,
}
pub trait EvidenceRepository {
    /// Both facts must come from one verified SQLite read snapshot.
    fn never_started_facts(&self, task_id: &str) -> Result<Facts, Error>;
}
pub struct NeverStarted<R> {
    pub repository: R,
}
fn supported(s: &TaskSnapshot) -> bool {
    let key = &s.task.key;
    key == &LegacyCollection.key()
        || (matches!(key.operation.as_str(), "update_data" | "backfill_data")
            && ((key.dataset == "market.daily"
                && key.contract_version == crate::market_update::tasks::CONTRACT_VERSION)
                || (key.dataset == "market.yearly"
                    && key.contract_version == crate::yearly_publication::tasks::CONTRACT_VERSION)))
}
impl<R: EvidenceRepository> StopInspector for NeverStarted<R> {
    fn inspect(&self, target: &TaskSnapshot) -> Result<Observation, Error> {
        if !supported(target)
            || target.task.status != Status::Pending
            || target.task.state_revision != 0
            || target.task.owner_run_id.is_some()
            || target.task.result.is_some()
            || target.items.is_empty()
            || target
                .items
                .iter()
                .any(|i| i.stage != Stage::Pending || i.outcome.is_some())
        {
            return Ok(Observation::Insufficient {
                reason:
                    "STOP_EVIDENCE_NOT_VERIFIED: task is not an audited never-started execution"
                        .into(),
            });
        }
        let facts = self.repository.never_started_facts(&target.task.task_id)?;
        if &facts.snapshot != target {
            return Err(Error::RevisionConflict);
        }
        if facts.has_durable_effects {
            return Err(Error::NeedsVerification(
                "never-started task has durable publication/result associations".into(),
            ));
        }
        Ok(Observation::Stopped {
            snapshot_sha256: snapshot_digest(target)?,
            evidence_ref: "sqlite-never-started-v1: revision-zero/no-owner/no-outcomes/no-effects; conditional commit excludes a later claim".into(),
        })
    }
}
