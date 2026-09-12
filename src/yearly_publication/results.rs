//! Application decision: publish a new yearly or record reuse without staging it.
use super::{YearlyCandidate, tasks};
use crate::market_storage::{
    Error, MarketRepository, Publication, PublishIntent, PublishPlan, Publisher, Receipt,
    compat::{self, CheckedRecord},
};
use serde_json::{Value, json};

#[derive(Debug, Clone)]
pub struct YearlyResultPlan {
    pub(crate) publication: PublishPlan,
    pub(crate) base: CheckedRecord,
    pub(crate) prior: Option<CheckedRecord>,
    pub(crate) no_change: bool,
    pub(crate) audit: Value,
}
impl YearlyResultPlan {
    pub fn is_no_change(&self) -> bool {
        self.no_change
    }
    pub fn command(&self) -> &Publication {
        self.publication.publication()
    }
}
pub trait YearlyRepository: MarketRepository {
    fn transition_task(
        &self,
        task: &str,
        revision: u64,
        run: &str,
        change: crate::task_control::Change,
    ) -> Result<crate::task_control::TaskSnapshot, Error>;
    fn commit_yearly(&self, plan: &YearlyResultPlan) -> Result<Receipt, Error>;
    fn yearly_receipt(&self, id: &str) -> Result<Option<Receipt>, Error>;
    fn yearly_item_receipt(&self, task: &str, item: &str) -> Result<Option<Receipt>, Error>;
}
impl<R: MarketRepository> Publisher<R> {
    pub fn prepare_yearly_result(
        &self,
        command: Publication,
        candidate: &YearlyCandidate,
        prior_id: Option<&str>,
    ) -> Result<YearlyResultPlan, Error> {
        tasks::validate_binding(
            &self.repository.task(&command.task_id)?,
            &command,
            candidate,
        )?;
        if candidate.raw["version_id"].as_str() != Some(command.version_id.as_str()) {
            return Err(Error::Invalid("yearly candidate identity".into()));
        }
        if let PublishIntent::CurrentHead {
            expected_yearly_head,
            ..
        } = &command.intent
            && expected_yearly_head.as_deref() != prior_id
        {
            return Err(Error::Conflict(
                "reuse comparison must bind expected yearly head".into(),
            ));
        }
        let base = self.repository.fixed(&candidate.base_daily_version_id)?;
        if base.raw()["integrity_sha256"] != candidate.base_integrity_sha256 {
            return Err(Error::Base("fixed base changed after derivation".into()));
        }
        let prior = prior_id.map(|id| self.repository.fixed(id)).transpose()?;
        let task = self.repository.task(&command.task_id)?;
        if task.task.key.contract_version == super::tasks::CONTRACT_VERSION {
            let comparison = prior.as_ref().map(|p|json!({"version_id":p.version().version_id,"integrity_sha256":p.raw()["integrity_sha256"]})).unwrap_or(Value::Null);
            if !task.items.iter().any(|i| {
                i.spec.item_id == command.item_id
                    && i.spec.frozen_inputs["comparison"] == comparison
            }) {
                return Err(Error::Conflict(
                    "comparison differs from accepted fixed version".into(),
                ));
            }
        }
        let next = compat::check(&candidate.raw)?;
        crate::market_storage::validate_base(&next, Some(&base))?;
        if prior.as_ref().is_some_and(|p| {
            p.version().symbol != next.version().symbol
                || p.version().period != next.version().period
        }) {
            return Err(Error::Base("prior yearly identity/period".into()));
        }
        let no_change = prior.as_ref().is_some_and(|p| equivalent(p, &next));
        let publication = if no_change {
            let prior = prior.as_ref().unwrap();
            let mut command = command;
            command.version_id = prior.version().version_id.clone();
            let mut plan = self.plan_record(command, prior.clone())?;
            plan.promote = None;
            plan
        } else {
            self.prepare_derived_yearly(command, candidate)?
        };
        let audit = json!({
            "format":"local-yearly-result-v1", "command":publication.command,
            "candidate":candidate.raw, "no_change":no_change,
            "base_daily_version_id":candidate.base_daily_version_id,
            "base_integrity_sha256":candidate.base_integrity_sha256,
            "prior_version_id":prior_id,
            "prior_integrity_sha256":prior.as_ref().map(|p| &p.raw()["integrity_sha256"]),
            "result_integrity_sha256":publication.record.raw()["integrity_sha256"],
        });
        Ok(YearlyResultPlan {
            publication,
            base,
            prior,
            no_change,
            audit,
        })
    }
}
pub(crate) fn equivalent(prior: &CheckedRecord, next: &CheckedRecord) -> bool {
    fn facts(r: &CheckedRecord) -> Option<Value> {
        let annual = r.raw().get("local_yearly_derivation_v1")?.get("annual")?;
        Some(json!({
            "symbol":r.version().symbol, "source":r.version().source,
            "adjustment":r.version().adjustment,
            "adjustment_evidence":r.version().adjustment_evidence,
            "units":r.version().unit_currency, "bars":r.version().records,
            "lineage":r.version().yearly_derivation,
            "coverage":r.batch.coverage,
            "annual_records":annual.get("records")?,
            "analysis_coverage_v1":r.raw().get("analysis_coverage_v1"),
        }))
    }
    match (facts(prior), facts(next)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}
