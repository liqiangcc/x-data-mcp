//! Bounded publication-aware application execution. No MCP or source selection.
use super::{
    DateRange, Decision, Evidence, PreparedRange, Reason,
    publication::{CombinedEvidence, Preparation, RangeRepository},
};
use crate::{
    market_data::{Bar, MarketDataBatch, MarketDataPipeline, SourceError},
    market_storage::{
        Error, Publication, PublishIntent, Publisher, Receipt, Scope, compat::CheckedRecord,
    },
    task_control::{Change, ItemOutcome, Stage, Status, TaskSnapshot},
};
use chrono::NaiveDate;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub struct CombinedCandidate {
    pub record: Value,
    pub evidence: CombinedEvidence,
}
/// Execution batch bound: the same ≤100 ceiling the explicit update
/// contract enforces on task acceptance. Chunked runners keep prepare,
/// commit and covering working sets proportional to the chunk — a
/// 5,224-item plan-run task peaks like a ≤100-item task instead of
/// materializing the whole batch (private-stack run peaked ~969MB
/// during `prepare_batch.base_reads` before this).
pub(crate) const EXEC_CHUNK: usize = 100;
pub struct Acquisition {
    pub batch: MarketDataBatch,
    pub evidence: Evidence,
    pub combined: Option<CombinedCandidate>,
}
/// Typed acquisition result for the ops-first update path (#259 S5G):
/// the continuity evidence plus exactly the bars the typed commit needs.
pub struct TypedAcquisition {
    pub decision: TypedDecision,
    pub evidence: Evidence,
}
pub enum TypedDecision {
    /// The member bytes and event set are byte-proven identical to the
    /// base's pinned snapshot inputs — publish a no-change receipt.
    Unchanged,
    /// The base's member bytes are a proven prefix of the current member;
    /// `upserts` are the qfq-adjusted in-range tail bars — pure appends.
    /// `metadata` carries the new generation/source-binding update when a
    /// cross-generation append must publish fresh version metadata (#320).
    Append {
        upserts: Vec<Bar>,
        metadata: Option<Box<crate::market_update::TypedVersionMetadata>>,
    },
}
/// Synchronous and read-only with respect to publication/task storage. Return
/// only after acquisition has stopped. Trusted fixture/verifier port, not MCP.
pub trait RangeProvider {
    fn acquire(
        &self,
        base: &CheckedRecord,
        range: &DateRange,
        candidate_id: &str,
    ) -> Result<Acquisition, SourceError>;
    /// Typed ops-first acquisition (#259 S5G). `Ok(None)` means the
    /// provider cannot serve the typed contract (or the update carries
    /// metadata outside the checked ops contract); the caller falls back
    /// to `acquire`.
    fn acquire_typed(
        &self,
        _facts: &super::UpdateBaseFacts,
        _base_window: &[Bar],
        _range: &DateRange,
    ) -> Result<Option<TypedAcquisition>, SourceError> {
        Ok(None)
    }
}
pub struct OfficialRangeProvider;
impl RangeProvider for OfficialRangeProvider {
    fn acquire(
        &self,
        base: &CheckedRecord,
        _range: &DateRange,
        _candidate_id: &str,
    ) -> Result<Acquisition, SourceError> {
        crate::official_files::OfficialFilePipeline
            .materialize_daily(&base.version().symbol, base.version().adjustment)?;
        Err(SourceError::NotVerified(
            "range evidence verifier and combined coverage are not implemented".into(),
        ))
    }
}
#[derive(Debug)]
pub enum ItemExecution {
    Published(Receipt),
    NotPublished(Box<TaskSnapshot>),
}
pub struct RangeRunner<R> {
    pub repository: R,
    pub run_id: String,
}
struct PlannedItem {
    base_id: String,
    range: DateRange,
    default_contract: bool,
}
impl<R: RangeRepository + Clone> RangeRunner<R> {
    pub fn run_item(
        &self,
        task_id: &str,
        item_id: &str,
        provider: &dyn RangeProvider,
    ) -> Result<ItemExecution, Error> {
        let mut binding = self.binding(task_id, item_id, "range")?;
        // Plan-run tasks carry the default frozen shape — the per-item work
        // is identical; only how the item set was derived differs.
        let default_contract = matches!(
            binding.head.key.contract_version.as_str(),
            super::tasks::DEFAULT_CONTRACT_VERSION | crate::data_supply::plan::RUN_CONTRACT_VERSION
        );
        let republish_contract =
            binding.head.key.contract_version == super::tasks::REPUBLISH_CONTRACT_VERSION;
        if !matches!(
            binding.head.key.operation.as_str(),
            "update_data" | "backfill_data" | "republish_data"
        ) || binding.head.key.dataset != "market.daily"
            || (!default_contract
                && !republish_contract
                && binding.head.key.contract_version != super::tasks::CONTRACT_VERSION)
        {
            return Err(Error::Invalid(
                "unsupported range execution contract".into(),
            ));
        }
        if let Some(receipt) = self.repository.item_receipt(task_id, item_id)? {
            // Covering write-back is idempotent and may lag the commit after
            // a crash; replay re-establishes it from the frozen gap list.
            if default_contract {
                self.ensure_default_coverings(
                    &binding.item.spec.frozen_inputs,
                    &receipt.version_id,
                )?;
            }
            return Ok(ItemExecution::Published(receipt));
        }
        if binding.head.visible_status(&self.run_id) == Status::NeedsVerification {
            return Err(Error::Task(crate::task_control::Error::NeedsVerification(
                "old run or unresolved execution".into(),
            )));
        }
        if binding.item.stage == Stage::Running {
            return Err(Error::OutcomeUnknown(
                "running item has no verified result; inspect before any retry".into(),
            ));
        }
        if binding.head.status.terminal() || binding.item.stage == Stage::Finished {
            if binding.item.outcome.as_ref().is_some_and(|o| o.success) {
                return Err(Error::Integrity(
                    "successful range item has no receipt".into(),
                ));
            }
            return self.not_published(task_id);
        }
        if binding.head.status == Status::CancelRequested {
            self.repository.transition_task_head(
                &mut binding.head,
                None,
                &self.run_id,
                Change::FinishExecution,
            )?;
            return self.not_published(task_id);
        }
        if binding.head.status == Status::Pending {
            self.repository.transition_task_head(
                &mut binding.head,
                None,
                &self.run_id,
                Change::Claim,
            )?;
        }
        self.repository.transition_task_head(
            &mut binding.head,
            Some(&mut binding.item),
            &self.run_id,
            Change::StartItem(item_id.into()),
        )?;
        let outcome = if default_contract {
            self.acquire_and_commit_default(&mut binding, item_id, provider)
        } else {
            self.acquire_and_commit(&mut binding, item_id, provider)
        };
        match outcome {
            Ok(receipt) => Ok(ItemExecution::Published(receipt)),
            Err(error) => {
                // A response can be lost after commit. Never overwrite success.
                if let Some(receipt) = self.repository.item_receipt(task_id, item_id)? {
                    if default_contract {
                        self.ensure_default_coverings(
                            &binding.item.spec.frozen_inputs,
                            &receipt.version_id,
                        )?;
                    }
                    return Ok(ItemExecution::Published(receipt));
                }
                if matches!(
                    error,
                    Error::OutcomeUnknown(_)
                        | Error::Storage(_)
                        | Error::Task(
                            crate::task_control::Error::StorageUnavailable(_)
                                | crate::task_control::Error::OutcomeUnknown(_)
                        )
                ) {
                    let mut binding = self.binding(task_id, item_id, "range")?;
                    if let Err(recording) = self.repository.transition_task_head(
                        &mut binding.head, Some(&mut binding.item), &self.run_id,
                        Change::RequireVerification {
                            item_id: item_id.into(),
                            reason: "item execution result not confirmed after acquisition/derivation/publication; committed receipt not yet established".into(),
                        },
                    ) {
                        // A concurrent commit may have won before the marker.
                        // Consult its fact once; never overwrite that success.
                        if let Some(receipt) = self.repository.item_receipt(task_id, item_id)? {
                            return Ok(ItemExecution::Published(receipt));
                        }
                        return Err(recording);
                    }
                    return Err(error);
                }
                let mut binding = self.binding(task_id, item_id, "range")?;
                let cancelled_without_publication = matches!(error, Error::Conflict(_))
                    && binding.head.status == Status::CancelRequested;
                self.repository.transition_task_head(
                    &mut binding.head,
                    Some(&mut binding.item),
                    &self.run_id,
                    Change::FinishItem(
                        item_id.into(),
                        ItemOutcome {
                            success: false,
                            error: Some(if cancelled_without_publication {
                                format!("cancelled before publication: {error}")
                            } else {
                                error.to_string()
                            }),
                            evidence: None,
                        },
                    ),
                )?;
                if binding.head.counts.finished == binding.head.counts.total
                    || binding.head.status == Status::CancelRequested
                {
                    self.repository.transition_task_head(
                        &mut binding.head,
                        None,
                        &self.run_id,
                        if cancelled_without_publication {
                            Change::FinishCancellation
                        } else {
                            Change::FinishExecution
                        },
                    )?;
                }
                self.not_published(task_id)
            }
        }
    }
    /// Bulk update path (#259 S5G-6): prepare every item outside the
    /// writer — binding checks plus typed acquisition (member hash,
    /// continuity probe, tail-only parse) — then commit the prepared set
    /// in ONE repository transaction via `commit_update_batch`. Items the
    /// typed contract cannot carry (replays, non-pending states,
    /// unverified members, unsupported backends) keep the per-item
    /// `run_item` path, whose reconciliation semantics are authoritative.
    /// `Ok(None)` means the repository lacks the batch surface; callers
    /// keep the per-item loop.
    pub fn run_items(
        &self,
        task_id: &str,
        item_ids: &[String],
        provider: &dyn RangeProvider,
    ) -> Result<Option<()>, Error> {
        let _total = crate::stage_trace::span("daily.run_items.total");
        let mut offset = 0;
        while offset < item_ids.len() {
            // Cancellation lands between chunks: committed chunks keep
            // their receipts, the remaining items take the per-item
            // path which owns the cancellation finish semantics.
            if offset > 0 && self.repository.task_head(task_id)?.status == Status::CancelRequested {
                break;
            }
            let chunk = &item_ids[offset..(offset + EXEC_CHUNK).min(item_ids.len())];
            let mut items = Vec::new();
            // Frozen inputs of prepared default-contract items: their gap
            // coverings append after the batch commit, per receipt.
            let mut coverings: Vec<(String, Value)> = Vec::new();
            let (prepared, mut fallback) = self.prepare_batch(task_id, chunk, provider);
            for (item, frozen) in prepared {
                if let Some(frozen) = frozen {
                    coverings.push((item.item_id.clone(), frozen));
                }
                items.push(item);
            }
            if !items.is_empty() {
                let commit_span = crate::stage_trace::span("daily.commit_batch");
                let committed = self.repository.commit_update_batch(task_id, &items);
                drop(commit_span);
                let results = match committed {
                    Ok(Some(results)) => Some(results),
                    // The first chunk's `None` keeps the documented
                    // contract — the repository lacks the batch surface
                    // and the caller runs the whole task per item.
                    Ok(None) if offset == 0 => return Ok(None),
                    // A mid-task `None`/structural error rolled the chunk
                    // back: those items keep the per-item path.
                    Ok(None) | Err(_) => {
                        fallback.extend(items.iter().map(|item| item.item_id.clone()));
                        None
                    }
                };
                if let Some(results) = results {
                    if results.len() != items.len() {
                        return Err(Error::Integrity(
                            "update batch result count does not match items".into(),
                        ));
                    }
                    for (item, result) in items.iter().zip(results) {
                        match result {
                            Ok(receipt) => {
                                if let Some((_, frozen)) =
                                    coverings.iter().find(|(id, _)| *id == item.item_id)
                                {
                                    self.ensure_default_coverings(frozen, &receipt.version_id)?;
                                }
                            }
                            Err(_) => fallback.push(item.item_id.clone()),
                        }
                    }
                }
            }
            for item_id in fallback {
                self.run_item(task_id, &item_id, provider)?;
            }
            offset += EXEC_CHUNK;
        }
        // Items past a cancellation break (or never reached) still take
        // the authoritative per-item path.
        for item_id in &item_ids[offset..] {
            self.run_item(task_id, item_id, provider)?;
        }
        Ok(Some(()))
    }
    /// Batch preparation for one update item (#259 S5G-6): the same
    /// pre-acquisition checks `run_item` performs, plus the typed
    /// acquire — no writes. `Ok(None)` defers the item to the per-item
    /// path; `Err` does the same through the caller's fallback list.
    /// The pair's second element carries the frozen inputs of
    /// default-contract items for post-commit gap coverings.
    /// `pub(crate)` for the capacity harness's phase timing (#259 S5G-7).
    pub(crate) fn update_candidate(
        &self,
        task_id: &str,
        item_id: &str,
        provider: &dyn RangeProvider,
    ) -> Result<Option<(crate::market_update::UpdateBatchItem, Option<Value>)>, Error> {
        let _total = crate::stage_trace::span("daily.prepare.item");
        let binding_span = crate::stage_trace::span("daily.prepare.binding");
        let binding = match self.repository.task_item_binding(task_id, item_id)? {
            Some(binding) => binding,
            None => {
                self.repository.task_head(task_id)?;
                return Err(Error::Invalid(
                    "range item absent from accepted task".into(),
                ));
            }
        };
        drop(binding_span);
        let receipt_span = crate::stage_trace::span("daily.prepare.item_receipt");
        let has_receipt = self.repository.item_receipt(task_id, item_id)?.is_some();
        drop(receipt_span);
        let Some(planned) = self.plan_candidate(&binding, has_receipt)? else {
            return Ok(None);
        };
        let facts_span = crate::stage_trace::span("daily.prepare.base_facts");
        let facts = self.repository.update_base_facts(&planned.base_id)?;
        drop(facts_span);
        let Some(facts) = facts else {
            return Ok(None);
        };
        let bars_span = crate::stage_trace::span("daily.prepare.base_bars");
        let base_window = self.repository.base_bars_in_range(
            &planned.base_id,
            planned.range.start,
            planned.range.end,
        )?;
        drop(bars_span);
        let Some(base_window) = base_window else {
            return Ok(None);
        };
        self.finish_candidate(
            task_id,
            item_id,
            &binding,
            planned,
            &facts,
            &base_window,
            provider,
        )
    }
    /// Batch preparation (XP-025 H4): item states and base reads for the
    /// whole task come from one read transaction each instead of five per
    /// item. Returns the prepared items plus the ids that must take the
    /// per-item path. A transaction-level read failure degrades to the
    /// original per-item `update_candidate` loop.
    #[allow(clippy::type_complexity)]
    pub(crate) fn prepare_batch(
        &self,
        task_id: &str,
        item_ids: &[String],
        provider: &dyn RangeProvider,
    ) -> (
        Vec<(crate::market_update::UpdateBatchItem, Option<Value>)>,
        Vec<String>,
    ) {
        let per_item = |ids: &[String]| {
            let mut prepared = Vec::new();
            let mut fallback = Vec::new();
            for item_id in ids {
                match self.update_candidate(task_id, item_id, provider) {
                    Ok(Some(candidate)) => prepared.push(candidate),
                    Ok(None) | Err(_) => fallback.push(item_id.clone()),
                }
            }
            (prepared, fallback)
        };
        let states_span = crate::stage_trace::span("daily.prepare_batch.states");
        let states = self.repository.range_item_states(task_id, item_ids);
        drop(states_span);
        let Ok(states) = states else {
            return per_item(item_ids);
        };
        if states.len() != item_ids.len() {
            return per_item(item_ids);
        }
        let mut fallback = Vec::new();
        let mut planned = Vec::new();
        for (item_id, state) in item_ids.iter().zip(states) {
            match state {
                Ok(crate::market_update::publication::RangeItemState {
                    binding: Some(binding),
                    has_receipt,
                }) => match self.plan_candidate(&binding, has_receipt) {
                    Ok(Some(plan)) => planned.push((item_id.clone(), binding, plan)),
                    Ok(None) | Err(_) => fallback.push(item_id.clone()),
                },
                Ok(_) | Err(_) => fallback.push(item_id.clone()),
            }
        }
        let wants: Vec<_> = planned
            .iter()
            .map(|(_, _, plan)| crate::market_update::publication::BaseWant {
                base_version_id: plan.base_id.clone(),
                start: plan.range.start,
                end: plan.range.end,
            })
            .collect();
        let reads_span = crate::stage_trace::span("daily.prepare_batch.base_reads");
        let reads = if wants.is_empty() {
            Ok(Vec::new())
        } else {
            self.repository.range_base_reads(&wants)
        };
        drop(reads_span);
        let reads = match reads {
            Ok(reads) if reads.len() == planned.len() => reads,
            _ => {
                let ids: Vec<String> = planned.iter().map(|(id, _, _)| id.clone()).collect();
                let (prepared, more) = per_item(&ids);
                fallback.extend(more);
                return (prepared, fallback);
            }
        };
        let mut prepared = Vec::new();
        for ((item_id, binding, plan), read) in planned.into_iter().zip(reads) {
            match read {
                Ok(Some(read)) => {
                    let acquire_span = crate::stage_trace::span("daily.prepare.item");
                    let candidate = self.finish_candidate(
                        task_id,
                        &item_id,
                        &binding,
                        plan,
                        &read.facts,
                        &read.bars,
                        provider,
                    );
                    drop(acquire_span);
                    match candidate {
                        Ok(Some(candidate)) => prepared.push(candidate),
                        Ok(None) | Err(_) => fallback.push(item_id),
                    }
                }
                Ok(None) | Err(_) => fallback.push(item_id),
            }
        }
        (prepared, fallback)
    }
    /// Pure pre-acquisition checks of one update item (everything
    /// `update_candidate` decides before reading the base). `Ok(None)`
    /// defers the item to the per-item path.
    fn plan_candidate(
        &self,
        binding: &crate::task_control::ItemBinding,
        has_receipt: bool,
    ) -> Result<Option<PlannedItem>, Error> {
        let default_contract = matches!(
            binding.head.key.contract_version.as_str(),
            super::tasks::DEFAULT_CONTRACT_VERSION | crate::data_supply::plan::RUN_CONTRACT_VERSION
        );
        let republish_contract =
            binding.head.key.contract_version == super::tasks::REPUBLISH_CONTRACT_VERSION;
        if !matches!(
            binding.head.key.operation.as_str(),
            "update_data" | "backfill_data" | "republish_data"
        ) || binding.head.key.dataset != "market.daily"
            || (!default_contract
                && !republish_contract
                && binding.head.key.contract_version != super::tasks::CONTRACT_VERSION)
        {
            return Err(Error::Invalid(
                "unsupported range execution contract".into(),
            ));
        }
        if has_receipt
            || binding.head.visible_status(&self.run_id) == Status::NeedsVerification
            || binding.item.stage != Stage::Pending
            || binding.head.status.terminal()
            || binding.head.status == Status::CancelRequested
        {
            return Ok(None);
        }
        let (base_id, range) = if default_contract {
            let parsed = super::tasks::parse_default_frozen(&binding.item.spec.frozen_inputs)?;
            match binding.head.key.operation.as_str() {
                "update_data" if parsed.refresh_range.is_none() => {
                    return Err(Error::Integrity(
                        "default update item missing frozen refresh range".into(),
                    ));
                }
                "backfill_data" if parsed.refresh_range.is_some() => {
                    return Err(Error::Integrity(
                        "default backfill item carries a refresh range".into(),
                    ));
                }
                _ => {}
            }
            (parsed.base_version_id.clone(), parsed.union()?)
        } else {
            let frozen = &binding.item.spec.frozen_inputs;
            let base_id = frozen["base_version_id"]
                .as_str()
                .ok_or_else(|| Error::Integrity("frozen base missing".into()))?
                .to_string();
            let range = DateRange {
                start: serde_json::from_value(frozen["range"]["start"].clone())
                    .map_err(|e| Error::Integrity(e.to_string()))?,
                end: serde_json::from_value(frozen["range"]["end"].clone())
                    .map_err(|e| Error::Integrity(e.to_string()))?,
            };
            (base_id, range)
        };
        Ok(Some(PlannedItem {
            base_id,
            range,
            default_contract,
        }))
    }
    /// Acquisition and typed-item assembly once the base facts and window
    /// are known (the part of `update_candidate` after the base reads).
    #[allow(clippy::too_many_arguments)]
    fn finish_candidate(
        &self,
        task_id: &str,
        item_id: &str,
        binding: &crate::task_control::ItemBinding,
        planned: PlannedItem,
        facts: &crate::market_update::UpdateBaseFacts,
        base_window: &[crate::market_data::Bar],
        provider: &dyn RangeProvider,
    ) -> Result<Option<(crate::market_update::UpdateBatchItem, Option<Value>)>, Error> {
        let PlannedItem {
            base_id,
            range,
            default_contract,
        } = planned;
        let acquire_span = crate::stage_trace::span("daily.prepare.acquire_typed");
        let acquired = provider
            .acquire_typed(facts, base_window, &range)
            .map_err(|e| Error::Invalid(e.to_string()))?;
        drop(acquire_span);
        let Some(acquired) = acquired else {
            return Ok(None);
        };
        // The batch transaction's revision CAS subsumes the per-item
        // post-acquisition task guard: any interleaving task change fails
        // the CAS and rolls the batch back to the per-item path.
        if acquired.evidence.base_version_id != base_id
            || acquired.evidence.base_integrity_sha256 != facts.integrity_sha256
            || acquired.evidence.range != range
        {
            return Err(Error::Invalid(format!(
                "NEEDS_VERIFICATION: {:?}",
                Reason::BasisUnknown
            )));
        }
        let id = crate::hash::sha256(serde_json::json!([task_id, item_id]).to_string().as_bytes());
        let scope = Scope {
            symbol: facts.symbol.clone(),
            period: crate::market_data::Period::Day,
        };
        let continuity = serde_json::to_value(&acquired.evidence).unwrap_or(Value::Null);
        let prepared = match acquired.decision {
            TypedDecision::Unchanged => crate::market_update::UpdateBatchPrepared::NoChange {
                base_version_id: base_id,
                range_start_day: crate::compact_market::epoch_day(range.start),
                range_end_day: crate::compact_market::epoch_day(range.end),
                continuity,
            },
            TypedDecision::Append { upserts, metadata } => {
                crate::market_update::UpdateBatchPrepared::Append {
                    base_version_id: base_id,
                    result_version_id: format!("range-version-{id}"),
                    upserts,
                    metadata,
                    continuity,
                }
            }
        };
        Ok(Some((
            crate::market_update::UpdateBatchItem {
                task_id: task_id.into(),
                item_id: item_id.into(),
                publication_id: format!("range-result-{id}"),
                run_id: self.run_id.clone(),
                symbol: facts.symbol.clone(),
                occupancy_key: scope.occupancy_key(),
                prepared,
            },
            default_contract.then(|| binding.item.spec.frozen_inputs.clone()),
        )))
    }
    /// O(1) head+item binding for this execution step; a missing item
    /// produces the same invalid-command error the snapshot path did.
    fn binding(
        &self,
        task_id: &str,
        item_id: &str,
        kind: &str,
    ) -> Result<crate::task_control::ItemBinding, Error> {
        match self.repository.task_item_binding(task_id, item_id)? {
            Some(binding) => Ok(binding),
            None => {
                self.repository.task_head(task_id)?;
                Err(Error::Invalid(format!(
                    "{kind} item absent from accepted task"
                )))
            }
        }
    }
    fn not_published(&self, task_id: &str) -> Result<ItemExecution, Error> {
        Ok(ItemExecution::NotPublished(Box::new(
            self.repository.task(task_id)?,
        )))
    }
    fn acquire_and_commit(
        &self,
        binding: &mut crate::task_control::ItemBinding,
        item_id: &str,
        provider: &dyn RangeProvider,
    ) -> Result<Receipt, Error> {
        let frozen = &binding.item.spec.frozen_inputs;
        let base_id = frozen["base_version_id"]
            .as_str()
            .ok_or_else(|| Error::Integrity("frozen base missing".into()))?
            .to_string();
        let base_digest = frozen["base_integrity_sha256"]
            .as_str()
            .ok_or_else(|| Error::Integrity("frozen base digest missing".into()))?
            .to_string();
        let range = DateRange {
            start: serde_json::from_value(frozen["range"]["start"].clone())
                .map_err(|e| Error::Integrity(e.to_string()))?,
            end: serde_json::from_value(frozen["range"]["end"].clone())
                .map_err(|e| Error::Integrity(e.to_string()))?,
        };
        self.commit_acquired_range(binding, item_id, &base_id, &base_digest, &range, provider)
    }
    /// Default-range execution (#147): every required range — refresh plus
    /// each frozen gap range for `update`, frozen gap ranges only for
    /// `backfill` — is satisfied by one fetch/commit over their union,
    /// through the same verified pipeline as the explicit contract. One
    /// commit means `finish_task` completes the item only after the union
    /// (and therefore every required range) is verified.
    fn acquire_and_commit_default(
        &self,
        binding: &mut crate::task_control::ItemBinding,
        item_id: &str,
        provider: &dyn RangeProvider,
    ) -> Result<Receipt, Error> {
        let parsed = super::tasks::parse_default_frozen(&binding.item.spec.frozen_inputs)?;
        // Shape/operation consistency: `update` freezes a refresh range,
        // `backfill` never does; anything else is a frozen-shape violation.
        match binding.head.key.operation.as_str() {
            "update_data" if parsed.refresh_range.is_none() => {
                return Err(Error::Integrity(
                    "default update item missing frozen refresh range".into(),
                ));
            }
            "backfill_data" if parsed.refresh_range.is_some() => {
                return Err(Error::Integrity(
                    "default backfill item carries a refresh range".into(),
                ));
            }
            _ => {}
        }
        let union = parsed.union()?;
        let receipt = self.commit_acquired_range(
            binding,
            item_id,
            &parsed.base_version_id,
            &parsed.base_integrity_sha256,
            &union,
            provider,
        )?;
        self.ensure_default_coverings(&binding.item.spec.frozen_inputs, &receipt.version_id)?;
        Ok(receipt)
    }
    /// Append covering bindings for frozen gaps the committed version
    /// satisfied in full. The exact covering condition: the gap is listed in
    /// the item's frozen `proven_gap_ranges` AND its entire
    /// `[start_date, end_date]` is contained in the committed range whose
    /// evidence the verified pipeline checked end-to-end. Gaps not in the
    /// frozen list never gain a binding, whatever their overlap.
    fn ensure_default_coverings(&self, frozen: &Value, version_id: &str) -> Result<(), Error> {
        let parsed = super::tasks::parse_default_frozen(frozen)?;
        let union = parsed.union()?;
        for gap in &parsed.proven_gap_ranges {
            let range = gap.range.range()?;
            if range.start < union.start || range.end > union.end {
                return Err(Error::Integrity(
                    "frozen gap outside committed verified range".into(),
                ));
            }
            // Replay: the identical binding already exists — cover_gap is
            // append-only, so skip rather than conflict on covered_at.
            if self
                .repository
                .gap_coverings(&gap.gap_id)?
                .iter()
                .any(|c| c.version_id == version_id)
            {
                continue;
            }
            self.repository
                .cover_gap(&crate::market_storage::GapCovering {
                    gap_id: gap.gap_id.clone(),
                    version_id: version_id.into(),
                    covered_at: chrono::Utc::now(),
                })?;
        }
        Ok(())
    }
    /// Verified pipeline shared by both contracts: provider acquisition,
    /// base/derivation checks, combined-coverage staging, publication
    /// verification and the atomic commit/receipt + finish_item.
    fn commit_acquired_range(
        &self,
        binding: &mut crate::task_control::ItemBinding,
        item_id: &str,
        base_id: &str,
        base_digest: &str,
        range: &DateRange,
        provider: &dyn RangeProvider,
    ) -> Result<Receipt, Error> {
        // Ops-first typed path (#259 S5G): metadata-only base facts,
        // tail-only member parse, O(ops) staged-delta commit — no
        // `fixed(base)` materialization and no combined envelope. A
        // `None` at any step means this update cannot be carried by the
        // typed contract (unsupported backend/provider, absent
        // continuity proof, or metadata drift); fall through to the
        // envelope path, which reproduces the same decisions at full
        // materialization cost.
        if let Some(receipt) =
            self.commit_typed_range(binding, item_id, base_id, base_digest, range, provider)?
        {
            return Ok(receipt);
        }
        let base = self.repository.fixed(base_id)?;
        if base.raw()["integrity_sha256"] != base_digest {
            return Err(Error::Integrity(
                "fixed base differs from accepted digest".into(),
            ));
        }
        let id = crate::hash::sha256(
            serde_json::json!([binding.head.task_id, item_id])
                .to_string()
                .as_bytes(),
        );
        let candidate_id = format!("range-version-{id}");
        let acquired = provider
            .acquire(&base, range, &candidate_id)
            .map_err(|e| Error::Invalid(e.to_string()))?;
        let current = self.repository.task_head(&binding.head.task_id)?;
        if current.status != Status::Running
            || current.state_revision != binding.head.state_revision
        {
            return Err(Error::Conflict(
                "task changed or cancelled during acquisition".into(),
            ));
        }
        let prepared =
            match super::prepare(&base, &acquired.batch, range, Some(&acquired.evidence))? {
                Decision::Candidate(prepared) => Some(prepared),
                Decision::NoChange { .. } => None,
                Decision::NeedsVerification(reason) => {
                    return Err(Error::Invalid(format!("NEEDS_VERIFICATION: {reason:?}")));
                }
            };
        if let Some(prepared) = &prepared {
            // Ops-first commit: a records-only merge (no coverage or unit
            // change) can be staged as a parent delta and bound by leaf
            // equality. Backends without an ops path fall back to the
            // envelope commit below.
            if !prepared.metadata_changed
                && let Some(receipt) = self.try_ops_commit(
                    binding,
                    item_id,
                    base_id,
                    &base,
                    prepared,
                    &acquired,
                    range,
                    &candidate_id,
                )?
            {
                return Ok(receipt);
            }
        }
        let changed = prepared.is_some();
        if changed {
            let whole = acquired.combined.as_ref().ok_or_else(|| {
                Error::Invalid("NEEDS_VERIFICATION: combined coverage absent".into())
            })?;
            if whole.record["version_id"] != candidate_id {
                return Err(Error::Invalid(
                    "unexpected candidate version identity".into(),
                ));
            }
            self.repository.stage(&whole.record)?;
        }
        let plan = (Publisher {
            repository: self.repository.clone(),
        })
        .prepare_range(
            Publication {
                publication_id: format!("range-result-{id}"),
                version_id: if changed {
                    candidate_id
                } else {
                    base_id.into()
                },
                task_id: binding.head.task_id.clone(),
                item_id: item_id.into(),
                run_id: self.run_id.clone(),
                expected_revision: binding.head.state_revision,
                intent: PublishIntent::Daily {
                    expected_head: Some(base_id.into()),
                },
            },
            &acquired.batch,
            range,
            &acquired.evidence,
            acquired.combined.as_ref(),
        )?;
        match plan {
            Preparation::Ready(plan) => self.repository.commit_range(&plan),
            Preparation::NeedsVerification(reason) => {
                Err(Error::Invalid(format!("NEEDS_VERIFICATION: {reason:?}")))
            }
        }
    }
    /// Typed ops-first range commit (#259 S5G): facts → probe →
    /// windowed read → O(ops) commit. Every validation the envelope
    /// path makes is preserved: frozen-inputs binding, the accepted
    /// base digest, in-range emptiness, unit/coverage parity, and the
    /// staged-ops read-back binding inside `commit_range_ops_typed`.
    /// `Ok(None)` at any step means the typed contract cannot carry
    /// this update and the caller continues on the envelope path.
    fn commit_typed_range(
        &self,
        binding: &crate::task_control::ItemBinding,
        item_id: &str,
        base_id: &str,
        base_digest: &str,
        range: &DateRange,
        provider: &dyn RangeProvider,
    ) -> Result<Option<Receipt>, Error> {
        let Some(facts) = self.repository.update_base_facts(base_id)? else {
            return Ok(None);
        };
        // The frozen `base_integrity_sha256` is a legacy envelope digest
        // that cannot be recomputed without materializing the base. On
        // this path the accepted-base binding is the compact integrity
        // digest instead — re-verified when `update_base_facts` read the
        // version row, and transitively covering the leaf content
        // (projection_digest) and pinned snapshot inputs
        // (source_binding_digest). The commit's head CAS and verified
        // source binding carry the rest of the envelope check's weight.
        let _ = base_digest;
        let Some(base_window) =
            self.repository
                .base_bars_in_range(base_id, range.start, range.end)?
        else {
            return Ok(None);
        };
        let Some(acquired) = provider
            .acquire_typed(&facts, &base_window, range)
            .map_err(|e| Error::Invalid(e.to_string()))?
        else {
            return Ok(None);
        };
        // Same post-acquisition task guard as the envelope path.
        let current = self.repository.task_head(&binding.head.task_id)?;
        if current.status != Status::Running
            || current.state_revision != binding.head.state_revision
        {
            return Err(Error::Conflict(
                "task changed or cancelled during acquisition".into(),
            ));
        }
        let id = crate::hash::sha256(
            serde_json::json!([binding.head.task_id, item_id])
                .to_string()
                .as_bytes(),
        );
        let command = Publication {
            publication_id: format!("range-result-{id}"),
            version_id: match &acquired.decision {
                TypedDecision::Append { .. } => format!("range-version-{id}"),
                TypedDecision::Unchanged => base_id.into(),
            },
            task_id: binding.head.task_id.clone(),
            item_id: item_id.into(),
            run_id: self.run_id.clone(),
            expected_revision: binding.head.state_revision,
            intent: PublishIntent::Daily {
                expected_head: Some(base_id.into()),
            },
        };
        // Typed frozen binding: the provider's evidence must claim the
        // same accepted base identity and range, and its integrity stamp
        // must equal the compact digest the repository verified when the
        // facts were read — this is the basis-binding role
        // `check_frozen_inputs` fills on the envelope path.
        if acquired.evidence.base_version_id != base_id
            || acquired.evidence.base_integrity_sha256 != facts.integrity_sha256
            || acquired.evidence.range != *range
        {
            return Err(Error::Invalid(format!(
                "NEEDS_VERIFICATION: {:?}",
                Reason::BasisUnknown
            )));
        }
        let continuity_evidence = serde_json::to_value(&acquired.evidence).ok();
        match &acquired.decision {
            TypedDecision::Unchanged => self.repository.commit_range_no_change_typed(
                &command,
                base_id,
                range,
                continuity_evidence.as_ref(),
            ),
            TypedDecision::Append { upserts, metadata } => {
                let scope = Scope {
                    symbol: facts.symbol.clone(),
                    period: crate::market_data::Period::Day,
                };
                self.repository.commit_range_ops_typed(
                    &command,
                    &scope,
                    base_id,
                    upserts,
                    &[],
                    metadata.as_deref(),
                    continuity_evidence.as_ref(),
                )
            }
        }
    }
    /// Ops-first commit for a records-only range merge on backends with
    /// parent-delta staging (#259). Runs the same accepted-inputs binding
    /// (`check_frozen_inputs`) and verifier base/segment digest checks as the
    /// envelope path; the candidate-content binding is the staged-leaf
    /// equality inside `commit_range_ops`, which replaces the envelope
    /// `combined_integrity_sha256` digest equality. `Ok(None)` falls back to
    /// the envelope stage/commit below.
    #[allow(clippy::too_many_arguments)]
    fn try_ops_commit(
        &self,
        binding: &crate::task_control::ItemBinding,
        item_id: &str,
        base_id: &str,
        base: &CheckedRecord,
        prepared: &PreparedRange,
        acquired: &Acquisition,
        range: &DateRange,
        candidate_id: &str,
    ) -> Result<Option<Receipt>, Error> {
        let command = Publication {
            publication_id: format!(
                "range-result-{}",
                crate::hash::sha256(
                    serde_json::json!([binding.head.task_id, item_id])
                        .to_string()
                        .as_bytes(),
                )
            ),
            version_id: candidate_id.into(),
            task_id: binding.head.task_id.clone(),
            item_id: item_id.into(),
            run_id: self.run_id.clone(),
            expected_revision: binding.head.state_revision,
            intent: PublishIntent::Daily {
                expected_head: Some(base_id.into()),
            },
        };
        (Publisher {
            repository: self.repository.clone(),
        })
        .check_frozen_inputs(&command, &acquired.evidence, range)?;
        let Some(whole) = &acquired.combined else {
            return Err(Error::Invalid(
                "NEEDS_VERIFICATION: combined coverage absent".into(),
            ));
        };
        if whole.record["version_id"] != candidate_id
            || whole.evidence.base_integrity_sha256 != prepared.base_integrity_sha256
            || whole.evidence.segment_sha256 != prepared.candidate_sha256
            || whole.evidence.complete_coverage_ref.trim().is_empty()
        {
            return Err(Error::Invalid(format!(
                "NEEDS_VERIFICATION: {:?}",
                Reason::CoverageUnknown
            )));
        }
        // !prepared.metadata_changed ⇒ the merged candidate reuses the base
        // coverage/units; the envelope path's unit-enrichment check is a
        // no-op on this path by construction.
        let base_dates: BTreeMap<NaiveDate, &Bar> =
            base.version().records.iter().map(|b| (b.date, b)).collect();
        let prepared_dates: BTreeSet<NaiveDate> = prepared.records.iter().map(|b| b.date).collect();
        let upserts: Vec<Bar> = prepared
            .records
            .iter()
            .filter(|b| base_dates.get(&b.date) != Some(b))
            .cloned()
            .collect();
        let removes: Vec<NaiveDate> = base_dates
            .keys()
            .filter(|d| !prepared_dates.contains(d))
            .copied()
            .collect();
        self.repository.commit_range_ops(
            &command,
            &Scope::of(base.version()),
            base_id,
            &upserts,
            &removes,
            &prepared.records,
        )
    }
}
