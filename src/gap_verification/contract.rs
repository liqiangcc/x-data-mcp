//! Acceptance contract for `verify_data_gap`: first acceptance freezes the
//! published QFQ daily subject inside one read snapshot; the task payload
//! stays the deterministic minimal `{"target":{...}}`.
use super::{GapDecisionStore, GapTarget, SubjectBinding, command_key};
use crate::task_control::{self as task, CommandKey, Contract, FrozenWork, ItemSpec};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;
fn invalid(message: impl ToString) -> task::Error {
    task::Error::InvalidCommand(message.to_string())
}
/// Normalized command intent: target only. deny_unknown_fields makes any
/// caller pass-through field (conclusion, digests, subject_version_*,
/// verifier identity, ...) a decode failure, not a stored field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GapIntent {
    target: IntentTarget,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IntentTarget {
    symbol: String,
    /// Day-only first phase; `year` (or anything else) fails decode.
    period: IntentPeriod,
    start_date: NaiveDate,
    end_date: NaiveDate,
}
/// Closed period literal; `year` never decodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum IntentPeriod {
    Day,
}
impl IntentTarget {
    fn valid_symbol(&self) -> bool {
        self.symbol.split_once('.').is_some_and(|(exchange, code)| {
            matches!(exchange, "sh" | "sz")
                && code.len() == 6
                && code.bytes().all(|b| b.is_ascii_digit())
        })
    }
    fn target(&self) -> Result<GapTarget, task::Error> {
        if !matches!(self.period, IntentPeriod::Day)
            || !self.valid_symbol()
            || self.start_date > self.end_date
        {
            return Err(invalid("invalid gap verification target"));
        }
        Ok(GapTarget {
            symbol: self.symbol.clone(),
            start_date: self.start_date,
            end_date: self.end_date,
        })
    }
}
/// Serde-stable frozen item evidence: the caller-named target and the
/// subject binding frozen from the acceptance snapshot.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GapFrozen {
    pub target: IntentTarget,
    pub subject_version_id: String,
    pub subject_version_digest: String,
}
impl GapFrozen {
    /// Rebuild the verified target/subject pair from frozen item evidence.
    /// Malformed or non-day evidence is an integrity failure, never a skip.
    pub(crate) fn parts(
        &self,
    ) -> Result<(GapTarget, SubjectBinding), crate::market_storage::Error> {
        let target = self.target.target().map_err(|_| {
            crate::market_storage::Error::Integrity("gap frozen target invalid".into())
        })?;
        Ok((
            target,
            SubjectBinding {
                version_id: self.subject_version_id.clone(),
                integrity_sha256: self.subject_version_digest.clone(),
            },
        ))
    }
}
/// Strictly decode the frozen item shape; malformed input is a storage
/// integrity failure, never a silent skip.
pub(crate) fn parse_gap_frozen(frozen: &Value) -> Result<GapFrozen, crate::market_storage::Error> {
    serde_json::from_value(frozen.clone())
        .map_err(|e| crate::market_storage::Error::Integrity(format!("gap frozen inputs: {e}")))
}
/// Application contract for `verify_data_gap`/`market.gap_verification`/
/// `gap-verification-task-v1`: one item per task, occupying the same
/// symbol x day resource as daily writes.
pub struct GapVerificationContract<S> {
    pub store: S,
}
impl<S: GapDecisionStore> Contract for GapVerificationContract<S> {
    fn key(&self) -> CommandKey {
        command_key()
    }
    fn normalize(&self, payload: Value) -> Result<Value, task::Error> {
        let intent: GapIntent = serde_json::from_value(payload).map_err(invalid)?;
        intent.target.target()?;
        serde_json::to_value(intent).map_err(invalid)
    }
    fn resolve(&self, intent: &Value) -> Result<FrozenWork, task::Error> {
        let intent: GapIntent = serde_json::from_value(intent.clone()).map_err(invalid)?;
        let target = intent.target.target()?;
        let subject: SubjectBinding = self
            .store
            .freeze_gap_subject(&target)
            .map_err(crate::market_update::tasks::repository_error)?;
        let scope = crate::task_control::legacy::scope_id(&target.symbol, "daily");
        let item = ItemSpec {
            item_id: target.symbol.clone(),
            scope_keys: BTreeSet::from([scope.clone()]),
            target: json!({"symbol":target.symbol,"period":"day"}),
            progress_unit: "evidence_check".into(),
            frozen_inputs: json!({
                "target": intent.target,
                "subject_version_id": subject.version_id,
                "subject_version_digest": subject.integrity_sha256,
            }),
        };
        Ok(FrozenWork {
            scopes: BTreeSet::from([scope]),
            items: vec![item],
        })
    }
}
