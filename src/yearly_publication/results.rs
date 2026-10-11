//! Application decision: publish a new yearly or record reuse without staging it.
use super::{YearlyCandidate, tasks};
use crate::market_storage::{
    Error, MarketRepository, Publication, PublishIntent, PublishPlan, Publisher, Receipt,
    compat::CheckedRecord,
};
use serde_json::{Value, json};

#[derive(Debug, Clone)]
pub struct YearlyResultPlan {
    pub(crate) publication: PublishPlan,
    pub(crate) base: CheckedRecord,
    pub(crate) prior: Option<CheckedRecord>,
    pub(crate) no_change: bool,
    pub(crate) audit: Value,
    pub(crate) audit_payloads: Vec<crate::sqlite_market::envelope::Payload>,
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
        self.prepare_yearly_result_prefetched(command, candidate, prior_id, None, None)
    }
    /// `prepare_yearly_result` with the caller's already-loaded item binding
    /// and base window — identical checks in the same order (#278): the same
    /// rows were previously re-read by `prepare_derived_yearly`, `plan_record`
    /// and `validate_base_facts` inside a single prepare call.
    pub(crate) fn prepare_yearly_result_prefetched(
        &self,
        command: Publication,
        candidate: &YearlyCandidate,
        prior_id: Option<&str>,
        binding: Option<&crate::task_control::ItemBinding>,
        base_meta: Option<&CheckedRecord>,
    ) -> Result<YearlyResultPlan, Error> {
        let fetched_binding;
        let binding = match binding {
            Some(binding) => Some(binding),
            None => {
                fetched_binding = self
                    .repository
                    .task_item_binding(&command.task_id, &command.item_id)?;
                fetched_binding.as_ref()
            }
        };
        match binding {
            Some(binding) => crate::stage_trace::timed("yearly.prepare.validate_binding", || {
                tasks::validate_binding(binding, &command, candidate)
            })?,
            None => {
                let head = self.repository.task_head(&command.task_id)?;
                if matches!(head.key.operation.as_str(), "update_data" | "backfill_data") {
                    return Err(Error::Conflict(
                        "yearly publication differs from accepted frozen inputs".into(),
                    ));
                }
            }
        }
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
        let base_facts = crate::stage_trace::timed("yearly.prepare.fixed_facts_base", || {
            self.repository
                .fixed_facts(&candidate.base_daily_version_id)
        })?;
        if base_facts.integrity_sha256 != candidate.base_integrity_sha256 {
            return Err(Error::Base("fixed base changed after derivation".into()));
        }
        // One-day window: full version metadata with a single-segment read
        // on compact backends; the legacy backend falls back to `fixed`,
        // which keeps `plan.base.raw` identical to the stored record its
        // commit path re-compares. A caller-supplied window serves the same
        // purpose when it covers the stored first day.
        let fetched_base;
        let base = match base_meta {
            Some(base)
                if base
                    .version()
                    .records
                    .iter()
                    .any(|r| r.date == base_facts.first_day) =>
            {
                base
            }
            _ => {
                fetched_base = crate::stage_trace::timed("yearly.prepare.fixed_window_1d", || {
                    self.repository.fixed_window(
                        &candidate.base_daily_version_id,
                        base_facts.first_day,
                        base_facts.first_day,
                    )
                })?;
                &fetched_base
            }
        };
        let prior = crate::stage_trace::timed("yearly.prepare.fixed_prior", || {
            // Approved layering difference (XP-032 P1): the prior head was
            // fully verified when it was published, so it is read at the
            // committed tier. The candidate and the commit transaction keep
            // full verification.
            prior_id
                .map(|id| self.repository.fixed_comparison(id))
                .transpose()
        })?;
        let binding = binding
            .ok_or_else(|| Error::Conflict("comparison item absent from accepted task".into()))?;
        if binding.head.key.contract_version == super::tasks::CONTRACT_VERSION {
            let comparison = prior.as_ref().map(|p|json!({"version_id":p.version().version_id,"integrity_sha256":p.raw()["integrity_sha256"]})).unwrap_or(Value::Null);
            if binding.item.spec.item_id != command.item_id
                || binding.item.spec.frozen_inputs["comparison"] != comparison
            {
                return Err(Error::Conflict(
                    "comparison differs from accepted fixed version".into(),
                ));
            }
        }
        let next = &candidate.checked;
        crate::stage_trace::timed("yearly.prepare.validate_base_facts_meta", || {
            crate::market_storage::validate_base_facts_meta(
                next,
                &self.repository,
                &base_facts,
                base,
            )
        })?;
        if prior.as_ref().is_some_and(|p| {
            p.version().symbol != next.version().symbol
                || p.version().period != next.version().period
        }) {
            return Err(Error::Base("prior yearly identity/period".into()));
        }
        let no_change = crate::stage_trace::timed("yearly.prepare.equivalent", || {
            prior.as_ref().is_some_and(|p| equivalent(p, next))
        });
        let publication = if no_change {
            let prior = prior.as_ref().unwrap();
            let mut command = command;
            command.version_id = prior.version().version_id.clone();
            // The prior's lineage may name a different base than the
            // candidate's — fetch facts/meta inside `plan_record` as before.
            let mut plan = crate::stage_trace::timed("yearly.prepare.plan_record", || {
                self.plan_record_prefetched(command, prior.clone(), Some(binding), None)
            })?;
            plan.promote = None;
            plan
        } else {
            crate::stage_trace::timed("yearly.prepare.derived", || {
                self.prepare_derived_yearly_prefetched(
                    command,
                    candidate,
                    Some(binding),
                    Some((&base_facts, base)),
                    prior.as_ref(),
                )
            })?
        };
        let (candidate_env, audit_payloads) =
            crate::stage_trace::timed("yearly.prepare.envelope_pack", || {
                crate::sqlite_market::envelope::pack(&candidate.raw)
            })?;
        let audit = json!({
            "format":"local-yearly-result-v1", "command":publication.command,
            "candidate":candidate_env, "no_change":no_change,
            "base_daily_version_id":candidate.base_daily_version_id,
            "base_integrity_sha256":candidate.base_integrity_sha256,
            "prior_version_id":prior_id,
            "prior_integrity_sha256":prior.as_ref().map(|p| &p.raw()["integrity_sha256"]),
            "result_integrity_sha256":publication.record.raw()["integrity_sha256"],
        });
        Ok(YearlyResultPlan {
            publication,
            base: base.clone(),
            prior,
            no_change,
            audit,
            audit_payloads,
        })
    }
}
/// Two yearly records are equivalent when they publish the same stored
/// content: the typed version fields, the yearly lineage binding, the
/// coverage verdict and the derivation `as_of` year. The derived
/// `local_yearly_derivation_v1` block itself is not compared — compact
/// storage omits it from published envelopes, and its contents (per-year
/// trading dates, unfinished flags, the annual records) are
/// deterministically re-aggregated from the bound base, years and as_of
/// that the compared fields already fix. `as_of` is matched at year
/// granularity: it only decides which year counts as unfinished, so a
/// same-year re-run is the same publication while a new year requires a
/// new version.
pub(crate) fn equivalent(prior: &CheckedRecord, next: &CheckedRecord) -> bool {
    fn facts(r: &CheckedRecord) -> Option<Value> {
        if r.version().period != crate::market_data::Period::Year {
            return None;
        }
        let as_of_year = r
            .raw()
            .pointer("/local_yearly_derivation_v1/annual/as_of")
            .or_else(|| r.raw().pointer("/compact_yearly_lineage_v1/as_of"))?
            .as_str()?
            .get(..4)?;
        Some(json!({
            "symbol":r.version().symbol, "source":r.version().source,
            "adjustment":r.version().adjustment,
            "adjustment_evidence":r.version().adjustment_evidence,
            "units":r.version().unit_currency, "bars":r.version().records,
            "lineage":r.version().yearly_derivation,
            "coverage":r.batch.coverage,
            "as_of_year":as_of_year,
            "analysis_coverage_v1":r.raw().get("analysis_coverage_v1"),
        }))
    }
    matches!((facts(prior), facts(next)), (Some(a), Some(b)) if a == b)
}
