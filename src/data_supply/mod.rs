//! Stage C application contracts; no network acquisition or implicit initialization.
pub mod derived;
pub mod wire;
use crate::market_storage::Error;
use std::collections::BTreeSet;
use wire::{Dataset, Security};
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Access {
    pub principal: String,
    pub datasets: BTreeSet<Dataset>,
    /// None grants all local securities; an empty set grants none.
    pub securities: Option<BTreeSet<Security>>,
}
impl Access {
    pub fn local() -> Self {
        Self {
            principal: "local-owner".into(),
            datasets: BTreeSet::from([
                Dataset::Catalog,
                Dataset::Daily,
                Dataset::Yearly,
                Dataset::Index,
                Dataset::AdjustmentEvents,
                Dataset::Calendar,
                // M2 facts (issue-391 §12.3): visible to the local owner like
                // the other research datasets.
                Dataset::SecurityMaster,
                Dataset::SecurityNames,
                Dataset::CapitalEvents,
                Dataset::RiskWarnings,
                Dataset::Suspensions,
                Dataset::PriceLimitRules,
            ]),
            securities: None,
        }
    }
    pub fn allows(&self, dataset: Dataset, entity: Option<&Security>) -> bool {
        self.datasets.contains(&dataset)
            && entity.is_none_or(|e| self.securities.as_ref().is_none_or(|s| s.contains(e)))
    }
    pub fn require(&self, dataset: Dataset, entity: Option<&Security>) -> Result<(), Error> {
        if self.allows(dataset, entity) {
            Ok(())
        } else {
            Err(Error::Invalid("forbidden".into()))
        }
    }
}

pub mod presence;
pub mod query;

pub mod columnar;

pub mod observe;

pub mod task_v2;

pub mod mcp;
pub mod registry;

pub mod collect;

pub mod default_range;
pub mod default_selection;
pub mod plan;

pub mod range;

pub mod generation;

pub mod worker;
