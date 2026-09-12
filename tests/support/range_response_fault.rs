//! Fault injection around real SQLite commit; no fake repository data/state.
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use x_data_mcp::{
    market_storage::{compat::CheckedRecord, *},
    market_update::publication::{RangePlan, RangeRepository},
    sqlite_market::SqliteMarket,
    task_control::{Change, TaskSnapshot},
};
#[derive(Clone)]
pub struct ResponseFault {
    pub db: SqliteMarket,
    pub commit_first: bool,
    pub fired: Arc<AtomicBool>,
}
impl MarketRepository for ResponseFault {
    fn stage(&self, v: &serde_json::Value) -> Result<(), Error> {
        self.db.stage(v)
    }
    fn candidate(&self, id: &str) -> Result<CheckedRecord, Error> {
        self.db.candidate(id)
    }
    fn mark_validated(&self, r: &CheckedRecord) -> Result<(), Error> {
        self.db.mark_validated(r)
    }
    fn fixed(&self, id: &str) -> Result<CheckedRecord, Error> {
        self.db.fixed(id)
    }
    fn fixed_many(&self, ids: &[String]) -> Result<Vec<CheckedRecord>, Error> {
        self.db.fixed_many(ids)
    }
    fn task(&self, id: &str) -> Result<TaskSnapshot, Error> {
        self.db.task(id)
    }
    fn commit(&self, p: &PublishPlan) -> Result<Receipt, Error> {
        self.db.commit(p)
    }
    fn receipt(&self, id: &str) -> Result<Option<Receipt>, Error> {
        self.db.receipt(id)
    }
    fn range(&self, s: &Scope, v: Option<&str>, r: &Range) -> Result<ReadPage, Error> {
        self.db.range(s, v, r)
    }
    fn versions(&self, s: &Scope) -> Result<Vec<x_data_mcp::publish::EffectiveVersion>, Error> {
        self.db.versions(s)
    }
}
impl RangeRepository for ResponseFault {
    fn commit_range(&self, p: &RangePlan) -> Result<Receipt, Error> {
        if !self.fired.swap(true, Ordering::SeqCst) {
            if self.commit_first {
                self.db.commit_range(p)?;
            }
            return Err(Error::OutcomeUnknown(
                "injected response uncertainty".into(),
            ));
        }
        self.db.commit_range(p)
    }
    fn range_receipt(&self, id: &str) -> Result<Option<Receipt>, Error> {
        self.db.range_receipt(id)
    }
    fn item_receipt(&self, t: &str, i: &str) -> Result<Option<Receipt>, Error> {
        self.db.item_receipt(t, i)
    }
    fn transition_task(
        &self,
        t: &str,
        r: u64,
        run: &str,
        c: Change,
    ) -> Result<TaskSnapshot, Error> {
        self.db.transition_task(t, r, run, c)
    }
}
