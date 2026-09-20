//! Proof for audited executors that claim an item before any write: if no item
//! is in-flight, every write has a determinate outcome. A pending item with no
//! execution-start marker never ran; a finished item's committed outcome is its
//! record. An in-flight item needs publication-result verification instead.
use super::{
    decision::{Observation, StopInspector, snapshot_digest},
    never_started,
};
use crate::task_control::{Contract, Error, Stage, TaskSnapshot, legacy::LegacyCollection};
use std::collections::BTreeSet;

pub struct Facts {
    pub snapshot: TaskSnapshot,
    /// Items with a committed StartItem marker (audited execution start).
    pub started_items: BTreeSet<String>,
    /// Items whose execution receipt committed. Every supported executor
    /// binds the per-item result row in the same transaction as the
    /// publication itself, so a row proves the write landed and its
    /// absence proves the publish commit never did.
    pub committed_items: BTreeSet<String>,
    /// Publication/result rows already bound to this task.
    pub has_durable_effects: bool,
}
pub trait EvidenceRepository {
    /// All facts must come from one verified SQLite read snapshot.
    fn determinate_facts(&self, task_id: &str) -> Result<Facts, Error>;
}
pub struct Determinate<R> {
    pub repository: R,
}
fn supported(s: &TaskSnapshot) -> bool {
    let key = &s.task.key;
    key == &LegacyCollection.key()
        || (key.operation == "start_collection"
            && key.contract_version == "v1"
            && matches!(
                key.dataset.as_str(),
                "market.daily" | "market.yearly" | "securities.catalog"
            ))
        || (matches!(key.operation.as_str(), "update_data" | "backfill_data")
            && ((key.dataset == "market.daily"
                && key.contract_version == crate::market_update::tasks::CONTRACT_VERSION)
                || (key.dataset == "market.yearly"
                    && key.contract_version == crate::yearly_publication::tasks::CONTRACT_VERSION)))
        || (key.operation == "republish_data"
            && key.dataset == "market.daily"
            && key.contract_version == crate::market_update::tasks::REPUBLISH_CONTRACT_VERSION)
}
impl<R: EvidenceRepository> StopInspector for Determinate<R> {
    fn inspect(&self, target: &TaskSnapshot) -> Result<Observation, Error> {
        if !supported(target) || target.items.is_empty() {
            return Ok(Observation::Insufficient {
                reason: "STOP_EVIDENCE_NOT_VERIFIED: task is not an audited determinate execution"
                    .into(),
            });
        }
        let facts = self.repository.determinate_facts(&target.task.task_id)?;
        if &facts.snapshot != target {
            return Err(Error::RevisionConflict);
        }
        let mut finished = 0usize;
        let mut landed = 0usize;
        let mut uncommitted = 0usize;
        for item in &target.items {
            let started = facts.started_items.contains(&item.spec.item_id);
            match item.stage {
                Stage::Finished => {
                    finished += 1;
                    if item.outcome.is_none() || !started {
                        return Ok(Observation::Insufficient {
                            reason: "STOP_EVIDENCE_NOT_VERIFIED: finished item lacks its audited start/outcome".into(),
                        });
                    }
                }
                Stage::Pending => {
                    if started {
                        return Ok(Observation::Insufficient {
                            reason: "STOP_EVIDENCE_NOT_VERIFIED: pending item has a prior execution start; its write outcome is unknown".into(),
                        });
                    }
                }
                Stage::Running => {
                    if !started {
                        return Ok(Observation::Insufficient {
                            reason: "STOP_EVIDENCE_NOT_VERIFIED: running item lacks its audited start marker".into(),
                        });
                    }
                    // The execution receipt commits atomically with the
                    // publication: a row means the write landed, no row
                    // means the publish commit never did. Both are
                    // determinate; the interrupted item re-runs or fails
                    // its base precondition honestly on retry.
                    if facts.committed_items.contains(&item.spec.item_id) {
                        landed += 1;
                    } else {
                        uncommitted += 1;
                    }
                }
            }
        }
        if finished + landed == 0 && facts.has_durable_effects {
            return Ok(Observation::Insufficient {
                reason: "STOP_EVIDENCE_NOT_VERIFIED: durable effects without a determinate write"
                    .into(),
            });
        }
        Ok(Observation::Stopped {
            snapshot_sha256: snapshot_digest(target)?,
            evidence_ref: format!(
                "sqlite-determinate-items-v1: {} finished items have audited start+outcome; {} pending items have no execution-start marker; {} in-flight items landed (committed receipt), {} in-flight items have no committed receipt; conditional commit binds the inspected snapshot",
                finished,
                target.items.len() - finished - landed - uncommitted,
                landed,
                uncommitted
            ),
        })
    }
}
/// The never-started proof first, then the started-but-determinate proof. Both
/// conclude from durable claim/outcome facts, never from process absence.
pub struct Audited<R> {
    pub repository: R,
}
impl<R> StopInspector for Audited<R>
where
    R: never_started::EvidenceRepository + EvidenceRepository + Clone,
{
    fn inspect(&self, target: &TaskSnapshot) -> Result<Observation, Error> {
        match (never_started::NeverStarted {
            repository: self.repository.clone(),
        })
        .inspect(target)?
        {
            stopped @ Observation::Stopped { .. } => Ok(stopped),
            Observation::Insufficient { .. } => Determinate {
                repository: self.repository.clone(),
            }
            .inspect(target),
        }
    }
}
