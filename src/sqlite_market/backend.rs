//! Runtime-selected market repository backend.
//!
//! `MarketBackend` dispatches on the durable schema version: schema 18
//! keeps the legacy `SqliteMarket`, schema 19 uses the compact
//! `SqliteCompactMarket`. The dispatch happens once at assembly so every
//! downstream contract stays generic over the trait surface.
use rusqlite::Connection;

use crate::market_storage::{Error, Receipt, compat::CheckedRecord};
use crate::sqlite_tasks::SqliteTasks;

use super::{SqliteMarket, compact_repo::SqliteCompactMarket};

/// Schema-selected market backend (legacy v18 or compact v19).
#[derive(Clone)]
pub enum MarketBackend {
    Legacy(SqliteMarket),
    Compact(SqliteCompactMarket),
}

impl MarketBackend {
    /// Opens the backend matching the durable schema version of `tasks`'s
    /// database file. An uninitialized or absent file keeps the legacy
    /// backend so a missing database fails closed per-operation instead of
    /// at service startup; explicit `upgrade_compact` produces schema 19.
    pub fn open(tasks: &SqliteTasks) -> Result<Self, Error> {
        if !tasks.path().is_file() {
            return Ok(Self::Legacy(SqliteMarket::new(tasks.clone())));
        }
        match tasks.schema_version()? {
            crate::sqlite_tasks::COMPACT_SCHEMA_VERSION => {
                Ok(Self::Compact(SqliteCompactMarket::open(tasks.path())?))
            }
            _ => Ok(Self::Legacy(SqliteMarket::new(tasks.clone()))),
        }
    }

    /// Task-store handle sharing the same database file.
    pub fn tasks(&self) -> SqliteTasks {
        match self {
            Self::Legacy(m) => m.tasks.clone(),
            Self::Compact(m) => m.tasks(),
        }
    }

    pub fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T, Error>) -> Result<T, Error> {
        match self {
            Self::Legacy(m) => m.read(f),
            Self::Compact(m) => m.read(f),
        }
    }
}

impl crate::market_storage::MarketRepository for MarketBackend {
    fn stage(&self, raw: &serde_json::Value) -> Result<(), Error> {
        match self {
            Self::Legacy(m) => m.stage(raw),
            Self::Compact(m) => m.stage(raw),
        }
    }
    fn candidate(&self, id: &str) -> Result<CheckedRecord, Error> {
        match self {
            Self::Legacy(m) => m.candidate(id),
            Self::Compact(m) => m.candidate(id),
        }
    }
    fn mark_validated(&self, record: &CheckedRecord) -> Result<(), Error> {
        match self {
            Self::Legacy(m) => m.mark_validated(record),
            Self::Compact(m) => m.mark_validated(record),
        }
    }
    fn fixed(&self, id: &str) -> Result<CheckedRecord, Error> {
        match self {
            Self::Legacy(m) => m.fixed(id),
            Self::Compact(m) => m.fixed(id),
        }
    }
    fn fixed_many(&self, ids: &[String]) -> Result<Vec<CheckedRecord>, Error> {
        match self {
            Self::Legacy(m) => m.fixed_many(ids),
            Self::Compact(m) => m.fixed_many(ids),
        }
    }
    fn task(&self, id: &str) -> Result<crate::task_control::TaskSnapshot, Error> {
        match self {
            Self::Legacy(m) => m.task(id),
            Self::Compact(m) => m.task(id),
        }
    }
    fn task_head(&self, id: &str) -> Result<crate::task_control::TaskHead, Error> {
        match self {
            Self::Legacy(m) => m.task_head(id),
            Self::Compact(m) => m.task_head(id),
        }
    }
    fn task_item_binding(
        &self,
        task: &str,
        item: &str,
    ) -> Result<Option<crate::task_control::ItemBinding>, Error> {
        match self {
            Self::Legacy(m) => m.task_item_binding(task, item),
            Self::Compact(m) => m.task_item_binding(task, item),
        }
    }
    fn transition_task_head(
        &self,
        head: &mut crate::task_control::TaskHead,
        item: Option<&mut crate::task_control::TaskItem>,
        run: &str,
        change: crate::task_control::Change,
    ) -> Result<(), Error> {
        match self {
            Self::Legacy(m) => m.transition_task_head(head, item, run, change),
            Self::Compact(m) => m.transition_task_head(head, item, run, change),
        }
    }
    fn commit(&self, plan: &crate::market_storage::PublishPlan) -> Result<Receipt, Error> {
        match self {
            Self::Legacy(m) => m.commit(plan),
            Self::Compact(m) => m.commit(plan),
        }
    }
    fn receipt(&self, id: &str) -> Result<Option<Receipt>, Error> {
        match self {
            Self::Legacy(m) => m.receipt(id),
            Self::Compact(m) => m.receipt(id),
        }
    }
    fn range(
        &self,
        scope: &crate::market_storage::Scope,
        version: Option<&str>,
        range: &crate::market_storage::Range,
    ) -> Result<crate::market_storage::ReadPage, Error> {
        match self {
            Self::Legacy(m) => m.range(scope, version, range),
            Self::Compact(m) => m.range(scope, version, range),
        }
    }
    fn versions(
        &self,
        scope: &crate::market_storage::Scope,
    ) -> Result<Vec<crate::publish::EffectiveVersion>, Error> {
        match self {
            Self::Legacy(m) => m.versions(scope),
            Self::Compact(m) => m.versions(scope),
        }
    }
}

impl crate::market_update::publication::RangeRepository for MarketBackend {
    fn commit_range(
        &self,
        plan: &crate::market_update::publication::RangePlan,
    ) -> Result<Receipt, Error> {
        match self {
            Self::Legacy(m) => m.commit_range(plan),
            Self::Compact(m) => m.commit_range(plan),
        }
    }
    fn commit_range_ops(
        &self,
        command: &crate::market_storage::Publication,
        scope: &crate::market_storage::Scope,
        base_version_id: &str,
        upserts: &[crate::market_data::Bar],
        removes: &[chrono::NaiveDate],
        expected: &[crate::market_data::Bar],
    ) -> Result<Option<Receipt>, Error> {
        match self {
            Self::Legacy(m) => {
                crate::market_update::publication::RangeRepository::commit_range_ops(
                    m,
                    command,
                    scope,
                    base_version_id,
                    upserts,
                    removes,
                    expected,
                )
            }
            Self::Compact(m) => {
                crate::market_update::publication::RangeRepository::commit_range_ops(
                    m,
                    command,
                    scope,
                    base_version_id,
                    upserts,
                    removes,
                    expected,
                )
            }
        }
    }
    fn update_base_facts(
        &self,
        base_version_id: &str,
    ) -> Result<Option<crate::market_update::UpdateBaseFacts>, Error> {
        match self {
            Self::Legacy(m) => {
                crate::market_update::publication::RangeRepository::update_base_facts(
                    m,
                    base_version_id,
                )
            }
            Self::Compact(m) => {
                crate::market_update::publication::RangeRepository::update_base_facts(
                    m,
                    base_version_id,
                )
            }
        }
    }
    fn base_bars_in_range(
        &self,
        base_version_id: &str,
        start: chrono::NaiveDate,
        end: chrono::NaiveDate,
    ) -> Result<Option<Vec<crate::market_data::Bar>>, Error> {
        match self {
            Self::Legacy(m) => {
                crate::market_update::publication::RangeRepository::base_bars_in_range(
                    m,
                    base_version_id,
                    start,
                    end,
                )
            }
            Self::Compact(m) => {
                crate::market_update::publication::RangeRepository::base_bars_in_range(
                    m,
                    base_version_id,
                    start,
                    end,
                )
            }
        }
    }
    fn commit_range_ops_typed(
        &self,
        command: &crate::market_storage::Publication,
        scope: &crate::market_storage::Scope,
        base_version_id: &str,
        upserts: &[crate::market_data::Bar],
        removes: &[chrono::NaiveDate],
        continuity_evidence: Option<&serde_json::Value>,
    ) -> Result<Option<Receipt>, Error> {
        match self {
            Self::Legacy(m) => {
                crate::market_update::publication::RangeRepository::commit_range_ops_typed(
                    m,
                    command,
                    scope,
                    base_version_id,
                    upserts,
                    removes,
                    continuity_evidence,
                )
            }
            Self::Compact(m) => {
                crate::market_update::publication::RangeRepository::commit_range_ops_typed(
                    m,
                    command,
                    scope,
                    base_version_id,
                    upserts,
                    removes,
                    continuity_evidence,
                )
            }
        }
    }
    fn commit_range_no_change_typed(
        &self,
        command: &crate::market_storage::Publication,
        base_version_id: &str,
        range: &crate::market_update::DateRange,
        continuity_evidence: Option<&serde_json::Value>,
    ) -> Result<Option<Receipt>, Error> {
        match self {
            Self::Legacy(m) => {
                crate::market_update::publication::RangeRepository::commit_range_no_change_typed(
                    m,
                    command,
                    base_version_id,
                    range,
                    continuity_evidence,
                )
            }
            Self::Compact(m) => {
                crate::market_update::publication::RangeRepository::commit_range_no_change_typed(
                    m,
                    command,
                    base_version_id,
                    range,
                    continuity_evidence,
                )
            }
        }
    }
    fn commit_update_batch(
        &self,
        task_id: &str,
        items: &[crate::market_update::UpdateBatchItem],
    ) -> Result<Option<Vec<Result<Receipt, Error>>>, Error> {
        match self {
            Self::Legacy(m) => {
                crate::market_update::publication::RangeRepository::commit_update_batch(
                    m, task_id, items,
                )
            }
            Self::Compact(m) => {
                crate::market_update::publication::RangeRepository::commit_update_batch(
                    m, task_id, items,
                )
            }
        }
    }
    fn range_receipt(&self, id: &str) -> Result<Option<Receipt>, Error> {
        match self {
            Self::Legacy(m) => m.range_receipt(id),
            Self::Compact(m) => m.range_receipt(id),
        }
    }
    fn item_receipt(&self, task: &str, item: &str) -> Result<Option<Receipt>, Error> {
        match self {
            Self::Legacy(m) => m.item_receipt(task, item),
            Self::Compact(m) => m.item_receipt(task, item),
        }
    }
    fn transition_task(
        &self,
        task: &str,
        revision: u64,
        run: &str,
        change: crate::task_control::Change,
    ) -> Result<crate::task_control::TaskSnapshot, Error> {
        match self {
            Self::Legacy(m) => crate::market_update::publication::RangeRepository::transition_task(
                m, task, revision, run, change,
            ),
            Self::Compact(m) => {
                crate::market_update::publication::RangeRepository::transition_task(
                    m, task, revision, run, change,
                )
            }
        }
    }
    fn cover_gap(&self, covering: &crate::market_storage::GapCovering) -> Result<bool, Error> {
        match self {
            Self::Legacy(m) => m.cover_gap(covering),
            Self::Compact(m) => m.cover_gap(covering),
        }
    }
    fn gap_coverings(
        &self,
        gap_id: &str,
    ) -> Result<Vec<crate::market_storage::GapCovering>, Error> {
        match self {
            Self::Legacy(m) => m.gap_coverings(gap_id),
            Self::Compact(m) => m.gap_coverings(gap_id),
        }
    }
}

impl crate::official_market::DailyCollectionRepository for MarketBackend {
    fn collection_receipt(&self, task: &str, item: &str) -> Result<Option<Receipt>, Error> {
        match self {
            Self::Legacy(m) => {
                crate::official_market::DailyCollectionRepository::collection_receipt(m, task, item)
            }
            Self::Compact(m) => {
                crate::official_market::DailyCollectionRepository::collection_receipt(m, task, item)
            }
        }
    }
    fn effective_head(
        &self,
        scope: &crate::market_storage::Scope,
    ) -> Result<Option<String>, Error> {
        match self {
            Self::Legacy(m) => {
                crate::official_market::DailyCollectionRepository::effective_head(m, scope)
            }
            Self::Compact(m) => {
                crate::official_market::DailyCollectionRepository::effective_head(m, scope)
            }
        }
    }
    fn transition_task(
        &self,
        task: &str,
        revision: u64,
        run: &str,
        change: crate::task_control::Change,
    ) -> Result<crate::task_control::TaskSnapshot, Error> {
        match self {
            Self::Legacy(m) => crate::official_market::DailyCollectionRepository::transition_task(
                m, task, revision, run, change,
            ),
            Self::Compact(m) => crate::official_market::DailyCollectionRepository::transition_task(
                m, task, revision, run, change,
            ),
        }
    }
    fn commit_collection_batch(
        &self,
        task_id: &str,
        items: &[crate::official_market::CollectionCandidate],
    ) -> Result<Option<Vec<Result<Receipt, Error>>>, Error> {
        match self {
            Self::Legacy(m) => {
                crate::official_market::DailyCollectionRepository::commit_collection_batch(
                    m, task_id, items,
                )
            }
            Self::Compact(m) => {
                crate::official_market::DailyCollectionRepository::commit_collection_batch(
                    m, task_id, items,
                )
            }
        }
    }
}

impl crate::publication_verification::decision::AuditStore for MarketBackend {
    fn commit_check(
        &self,
        plan: &crate::publication_verification::decision::Plan,
    ) -> Result<crate::publication_verification::decision::Receipt, Error> {
        match self {
            Self::Legacy(m) => {
                crate::publication_verification::decision::AuditStore::commit_check(m, plan)
            }
            Self::Compact(m) => {
                crate::publication_verification::decision::AuditStore::commit_check(m, plan)
            }
        }
    }
    fn check_receipt(
        &self,
        id: &str,
    ) -> Result<Option<crate::publication_verification::decision::Receipt>, Error> {
        match self {
            Self::Legacy(m) => {
                crate::publication_verification::decision::AuditStore::check_receipt(m, id)
            }
            Self::Compact(m) => {
                crate::publication_verification::decision::AuditStore::check_receipt(m, id)
            }
        }
    }
}

impl crate::publication_verification::EvidenceRepository for MarketBackend {
    fn publication_facts(
        &self,
        task_id: &str,
    ) -> Result<crate::publication_verification::Facts, Error> {
        match self {
            Self::Legacy(m) => m.publication_facts(task_id),
            Self::Compact(m) => m.publication_facts(task_id),
        }
    }
}

impl crate::yearly_publication::results::YearlyRepository for MarketBackend {
    fn transition_task(
        &self,
        task: &str,
        revision: u64,
        run: &str,
        change: crate::task_control::Change,
    ) -> Result<crate::task_control::TaskSnapshot, Error> {
        match self {
            Self::Legacy(m) => {
                crate::yearly_publication::results::YearlyRepository::transition_task(
                    m, task, revision, run, change,
                )
            }
            Self::Compact(m) => {
                crate::yearly_publication::results::YearlyRepository::transition_task(
                    m, task, revision, run, change,
                )
            }
        }
    }
    fn commit_yearly(
        &self,
        plan: &crate::yearly_publication::results::YearlyResultPlan,
    ) -> Result<Receipt, Error> {
        match self {
            Self::Legacy(m) => m.commit_yearly(plan),
            Self::Compact(m) => m.commit_yearly(plan),
        }
    }
    fn yearly_receipt(&self, id: &str) -> Result<Option<Receipt>, Error> {
        match self {
            Self::Legacy(m) => m.yearly_receipt(id),
            Self::Compact(m) => m.yearly_receipt(id),
        }
    }
    fn yearly_item_receipt(&self, task: &str, item: &str) -> Result<Option<Receipt>, Error> {
        match self {
            Self::Legacy(m) => m.yearly_item_receipt(task, item),
            Self::Compact(m) => m.yearly_item_receipt(task, item),
        }
    }
}

impl crate::data_supply::default_range::DefaultRangeSource for MarketBackend {
    fn resolve_default_work(
        &self,
        op: crate::data_supply::default_range::DefaultOperation,
        today: chrono::NaiveDate,
    ) -> Result<crate::data_supply::default_range::DefaultWork, Error> {
        match self {
            Self::Legacy(m) => {
                crate::data_supply::default_range::DefaultRangeSource::resolve_default_work(
                    m, op, today,
                )
            }
            Self::Compact(m) => {
                crate::data_supply::default_range::DefaultRangeSource::resolve_default_work(
                    m, op, today,
                )
            }
        }
    }
}

impl crate::gap_verification::runner::GapTaskStore for MarketBackend {
    fn task(&self, id: &str) -> Result<crate::task_control::TaskSnapshot, Error> {
        crate::market_storage::MarketRepository::task(self, id)
    }
    fn transition_task(
        &self,
        task: &str,
        revision: u64,
        run: &str,
        change: crate::task_control::Change,
    ) -> Result<crate::task_control::TaskSnapshot, Error> {
        match self {
            Self::Legacy(m) => crate::gap_verification::runner::GapTaskStore::transition_task(
                m, task, revision, run, change,
            ),
            Self::Compact(m) => crate::gap_verification::runner::GapTaskStore::transition_task(
                m, task, revision, run, change,
            ),
        }
    }
}

impl crate::gap_verification::GapDecisionStore for MarketBackend {
    fn freeze_gap_subject(
        &self,
        target: &crate::gap_verification::GapTarget,
    ) -> Result<crate::gap_verification::SubjectBinding, Error> {
        match self {
            Self::Legacy(m) => {
                crate::gap_verification::GapDecisionStore::freeze_gap_subject(m, target)
            }
            Self::Compact(m) => {
                crate::gap_verification::GapDecisionStore::freeze_gap_subject(m, target)
            }
        }
    }
    fn check_gap_subject(
        &self,
        subject: &crate::gap_verification::SubjectBinding,
    ) -> Result<(), Error> {
        match self {
            Self::Legacy(m) => {
                crate::gap_verification::GapDecisionStore::check_gap_subject(m, subject)
            }
            Self::Compact(m) => {
                crate::gap_verification::GapDecisionStore::check_gap_subject(m, subject)
            }
        }
    }
    fn commit_gap_decision(
        &self,
        decision: &crate::market_storage::GapVerificationDecision,
    ) -> Result<bool, Error> {
        match self {
            Self::Legacy(m) => {
                crate::gap_verification::GapDecisionStore::commit_gap_decision(m, decision)
            }
            Self::Compact(m) => {
                crate::gap_verification::GapDecisionStore::commit_gap_decision(m, decision)
            }
        }
    }
    fn gap_decision(
        &self,
        decision_id: &str,
    ) -> Result<Option<crate::market_storage::GapVerificationDecision>, Error> {
        match self {
            Self::Legacy(m) => {
                crate::gap_verification::GapDecisionStore::gap_decision(m, decision_id)
            }
            Self::Compact(m) => {
                crate::gap_verification::GapDecisionStore::gap_decision(m, decision_id)
            }
        }
    }
}
