//! Stage B application contracts. No SQLite, MCP, download or production wiring.
pub mod compat;
pub mod query;
use crate::{
    market_data::{Bar, Period},
    publish::EffectiveVersion,
    task_control::{self, Change, ItemOutcome, Stage, Status, TaskSnapshot},
};
use chrono::{Datelike, FixedOffset, NaiveDate, Utc};
use compat::CheckedRecord;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("MARKET_STORAGE: {0}")]
    Storage(String),
    #[error("MARKET_INTEGRITY: {0}")]
    Integrity(String),
    #[error("BASE_UNAVAILABLE: {0}")]
    Base(String),
    #[error("PUBLISH_PRECONDITION: {0}")]
    Conflict(String),
    #[error("MARKET_NOT_FOUND")]
    NotFound,
    #[error("INVALID_MARKET_COMMAND: {0}")]
    Invalid(String),
    #[error("PUBLISH_OUTCOME_UNKNOWN: {0}; inspect the same publication_id")]
    OutcomeUnknown(String),
    #[error(transparent)]
    Task(#[from] task_control::Error),
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    pub symbol: String,
    pub period: Period,
}
impl Scope {
    pub fn of(v: &EffectiveVersion) -> Self {
        Self {
            symbol: v.symbol.clone(),
            period: v.period,
        }
    }
    pub fn occupancy_key(&self) -> String {
        task_control::legacy::scope_id(
            &self.symbol,
            match self.period {
                Period::Day => "day",
                Period::Year => "year",
            },
        )
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PublishIntent {
    Daily {
        expected_head: Option<String>,
    },
    HistoricalFixedBase,
    CurrentHead {
        expected_daily_head: String,
        expected_yearly_head: Option<String>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Publication {
    pub publication_id: String,
    pub version_id: String,
    pub task_id: String,
    pub item_id: String,
    pub run_id: String,
    pub expected_revision: u64,
    pub intent: PublishIntent,
}
/// Sealed application decision; infrastructure cannot independently choose promotion.
#[derive(Debug, Clone)]
pub struct PublishPlan {
    pub(crate) command: Publication,
    pub(crate) record: CheckedRecord,
    pub(crate) required_heads: Vec<(Scope, Option<String>)>,
    pub(crate) promote: Option<Scope>,
    pub(crate) base_digest: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub publication_id: String,
    pub version_id: String,
    pub task_id: String,
    pub task_revision: u64,
}
#[derive(Debug, Clone)]
pub struct ReadPage {
    pub integrity_sha256: String,
    pub version: EffectiveVersion,
    pub rows: Vec<Bar>,
    pub next_after: Option<NaiveDate>,
}
#[derive(Debug, Clone)]
pub struct Range {
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub after: Option<NaiveDate>,
    pub limit: usize,
}
impl Range {
    pub fn validate(&self) -> Result<(), Error> {
        if self.start.year() <= 0
            || self.end.year() > 9999
            || self.start > self.end
            || self.limit == 0
            || self.limit > 1000
        {
            return Err(Error::Invalid("date range/page size (1..1000)".into()));
        }
        Ok(())
    }
}
pub trait MarketRepository {
    fn stage(&self, raw: &Value) -> Result<(), Error>;
    fn candidate(&self, id: &str) -> Result<CheckedRecord, Error>;
    fn mark_validated(&self, record: &CheckedRecord) -> Result<(), Error>;
    fn fixed(&self, id: &str) -> Result<CheckedRecord, Error>;
    fn fixed_many(&self, ids: &[String]) -> Result<Vec<CheckedRecord>, Error>;
    fn task(&self, id: &str) -> Result<TaskSnapshot, Error>;
    fn commit(&self, plan: &PublishPlan) -> Result<Receipt, Error>;
    fn receipt(&self, id: &str) -> Result<Option<Receipt>, Error>;
    fn range(&self, scope: &Scope, version: Option<&str>, range: &Range)
    -> Result<ReadPage, Error>;
    fn versions(&self, scope: &Scope) -> Result<Vec<EffectiveVersion>, Error>;
}
pub struct Publisher<R> {
    pub repository: R,
}
impl<R: MarketRepository> Publisher<R> {
    pub fn prepare(&self, command: Publication) -> Result<PublishPlan, Error> {
        let record = self.repository.candidate(&command.version_id)?;
        let plan = self.plan_record(command, record)?;
        self.repository.mark_validated(&plan.record)?;
        Ok(plan)
    }
    /// Shared application eligibility for a staged candidate or a verified reuse.
    pub(crate) fn plan_record(
        &self,
        command: Publication,
        record: CheckedRecord,
    ) -> Result<PublishPlan, Error> {
        if command.publication_id.trim().is_empty() {
            return Err(Error::Invalid("empty publication_id".into()));
        }
        let v = &record.version;
        let scope = Scope::of(v);
        let (required_heads, promote) = match (&command.intent, v.period) {
            (PublishIntent::Daily { expected_head }, Period::Day) => {
                (vec![(scope.clone(), expected_head.clone())], Some(scope))
            }
            (PublishIntent::HistoricalFixedBase, Period::Year) => (vec![], None),
            (
                PublishIntent::CurrentHead {
                    expected_daily_head,
                    expected_yearly_head,
                },
                Period::Year,
            ) => {
                if v.yearly_derivation
                    .as_ref()
                    .map(|l| l.base_daily_version_id.as_str())
                    != Some(expected_daily_head)
                {
                    return Err(Error::Conflict(
                        "candidate must bind expected daily head".into(),
                    ));
                }
                (
                    vec![
                        (
                            Scope {
                                symbol: v.symbol.clone(),
                                period: Period::Day,
                            },
                            Some(expected_daily_head.clone()),
                        ),
                        (scope.clone(), expected_yearly_head.clone()),
                    ],
                    Some(scope),
                )
            }
            _ => return Err(Error::Invalid("intent/period mismatch".into())),
        };
        let mut plan = PublishPlan {
            command,
            record,
            required_heads,
            promote,
            base_digest: None,
        };
        let base = if let Some(lineage) = &plan.record.version.yearly_derivation {
            Some(
                self.repository
                    .fixed(&lineage.base_daily_version_id)
                    .map_err(base_error)?,
            )
        } else {
            None
        };
        validate_base(&plan.record, base.as_ref())?;
        plan.base_digest = base
            .as_ref()
            .map(|r| r.raw["integrity_sha256"].as_str().unwrap().to_owned());
        plan.finish_task(self.repository.task(&plan.command.task_id)?)?;
        Ok(plan)
    }
}
impl PublishPlan {
    pub fn publication(&self) -> &Publication {
        &self.command
    }
    pub fn head_preconditions(&self) -> &[(Scope, Option<String>)] {
        &self.required_heads
    }
    pub fn head_promotion(&self) -> Option<&Scope> {
        self.promote.as_ref()
    }
    /// Same pure decision is re-evaluated against facts acquired inside commit.
    pub(crate) fn finish_task(&self, task: TaskSnapshot) -> Result<TaskSnapshot, Error> {
        if task.task.state_revision != self.command.expected_revision
            || task.visible_status(&self.command.run_id) != Status::Running
            || task.task.owner_run_id.as_deref() != Some(&self.command.run_id)
        {
            return Err(Error::Conflict("task revision/owner/state".into()));
        }
        let scope = Scope::of(&self.record.version);
        let item = task
            .items
            .iter()
            .find(|i| i.spec.item_id == self.command.item_id)
            .ok_or_else(|| Error::Conflict("missing task item".into()))?;
        if item.stage != Stage::Running
            || item.spec.target
                != serde_json::json!({"symbol":scope.symbol,"period":match scope.period{Period::Day=>"day",Period::Year=>"year"}})
            || !item.spec.scope_keys.contains(&scope.occupancy_key())
            || !task.claims.iter().any(|c| {
                c.scope_key == scope.occupancy_key()
                    && c.task_id == self.command.task_id
                    && c.state_revision == self.command.expected_revision
            })
        {
            return Err(Error::Conflict("task target/occupancy".into()));
        }
        let mut next = task_control::decide(
            task,
            &self.command.run_id,
            Change::FinishItem(
                self.command.item_id.clone(),
                ItemOutcome {
                    success: true,
                    error: None,
                },
            ),
            Utc::now(),
        )?;
        if next.items.iter().all(|i| i.stage == Stage::Finished) {
            next = task_control::decide(
                next,
                &self.command.run_id,
                Change::FinishExecution,
                Utc::now(),
            )?;
        }
        Ok(next)
    }
}
pub(crate) fn base_error(e: Error) -> Error {
    Error::Base(e.to_string())
}
pub(crate) fn validate_base_identity(
    record: &CheckedRecord,
    base: Option<&CheckedRecord>,
) -> Result<(), Error> {
    let v = &record.version;
    if v.period == Period::Day {
        return Ok(());
    }
    let lineage = v
        .yearly_derivation
        .as_ref()
        .ok_or_else(|| Error::Base("yearly lineage missing".into()))?;
    let daily = &base
        .ok_or_else(|| Error::Base("fixed base missing".into()))?
        .version;
    if daily.version_id != lineage.base_daily_version_id
        || daily.symbol != v.symbol
        || daily.period != Period::Day
        || daily.adjustment != v.adjustment
        || daily.source != v.source
        || daily.unit_currency != v.unit_currency
        || !daily.adjustment_evidence.is_confirmed()
        || daily.adjustment_evidence.value.as_deref() != Some("qfq")
    {
        return Err(Error::Base(
            "fixed base identity/period/adjustment/source mismatch".into(),
        ));
    }
    Ok(())
}
pub(crate) fn validate_base(
    record: &CheckedRecord,
    base: Option<&CheckedRecord>,
) -> Result<(), Error> {
    validate_base_identity(record, base)?;
    let v = &record.version;
    if v.period == Period::Day {
        return Ok(());
    }
    let lineage = v.yearly_derivation.as_ref().unwrap();
    let daily = &base.unwrap().version;
    let years = v.records.iter().map(|b| b.date.year()).collect();
    let as_of = v
        .published_at
        .with_timezone(&FixedOffset::east_opt(8 * 3600).unwrap())
        .date_naive();
    let annual = crate::yearly::aggregate(daily, Some(&years), as_of)
        .map_err(|e| Error::Base(e.to_string()))?;
    if annual
        .records
        .iter()
        .map(|r| r.bar.clone())
        .collect::<Vec<_>>()
        != v.records
        || lineage.algorithm_version != crate::yearly::ALGORITHM_VERSION
        || annual.records.first().map(|r| r.lineage.first_trading_date)
            != Some(lineage.first_trading_date)
        || annual.records.last().map(|r| r.lineage.last_trading_date)
            != Some(lineage.last_trading_date)
    {
        return Err(Error::Base(
            "yearly content/lineage does not match fixed daily".into(),
        ));
    }
    Ok(())
}
