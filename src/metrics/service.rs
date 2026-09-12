//! Fixed-input use case. Permission and contract checks precede any storage read.
use super::{
    consecutive::{Observation, evaluate},
    wire::*,
};
use crate::{
    data_supply::{
        Access,
        wire::{Dataset, VersionRef},
    },
    market_storage::Error,
};
pub trait InputRepository {
    fn observations(
        &self,
        reference: &VersionRef,
        range: &Years,
    ) -> Result<Vec<Observation>, Error>;
}
pub struct Metrics<R> {
    pub repository: R,
}
impl<R: InputRepository> Metrics<R> {
    pub fn compute(&self, request: Compute, access: &Access) -> Result<Computation, Error> {
        if request.inputs.len() != 1 || request.metrics.len() != 1 {
            return Err(Error::Invalid(
                "exactly one inline input and metric supported".into(),
            ));
        }
        let input = &request.inputs[0];
        let metric = &request.metrics[0];
        if input.input_id.trim().is_empty()
            || metric.metric_id.trim().is_empty()
            || metric.input_ids != vec![input.input_id.clone()]
            || input.version_ref.version_id.trim().is_empty()
            || input.version_ref.version_id == "latest"
            || input.version_ref.integrity_sha256.len() != 64
        {
            return Err(Error::Invalid(
                "fixed yearly identity/digest/input reference required".into(),
            ));
        }
        let entity = &input.version_ref.entity;
        entity.validate().map_err(Error::Invalid)?;
        access.require(Dataset::Yearly, Some(entity))?;
        let range = &request.range;
        if range.start_year < 1
            || range.end_year > 9999
            || range.end_year < range.start_year
            || range.end_year - range.start_year >= 1000
            || !(1..=1000).contains(&metric.parameters.min_periods)
        {
            return Err(Error::Invalid("computation resource/range budget".into()));
        }
        let observations = self
            .repository
            .observations(&input.version_ref.reference(), range)?;
        let mut result = evaluate(&observations, range, &metric.parameters)?;
        let (Outcome::Computed { limitations, .. } | Outcome::NotComputable { limitations, .. }) =
            &mut result;
        limitations
            .push("completion evidence is fixture-only; no production source approval".into());
        Ok(Computation {
            contract_version: request.contract_version,
            input: input.clone(),
            metric: metric.clone(),
            requested_range: request.range,
            result,
        })
    }
}
