//! `get_yearly_base_years` application decisions (XP-014, generic-data-v5):
//! request-level validation completes before any version content is read;
//! the repository resolves every item inside ONE read snapshot. MCP maps
//! the wire contract only.

use super::{
    Access,
    query::{MAX_BYTES, invalid},
    wire::*,
};
use crate::market_storage::Error;
use chrono::Utc;

/// Validated batch: sorted unique entities, checked bounds, authorization
/// preflight done — the repository may resolve these in any order.
#[derive(Clone)]
pub struct PresencePlan {
    pub items: Vec<YearPresenceItem>,
}

impl PresencePlan {
    pub fn new(mut args: YearPresenceQuery, access: &Access) -> Result<Self, Error> {
        if args.items.is_empty() || args.items.len() > 100 {
            return Err(invalid("items must contain 1..100 bindings"));
        }
        for item in &args.items {
            item.entity.validate().map_err(Error::Invalid)?;
            if item.base_daily_version_id.trim().is_empty()
                || !(1..=9999).contains(&item.start_year)
                || !(1..=9999).contains(&item.end_year)
                || item.start_year > item.end_year
            {
                return Err(invalid("invalid entity/version/year binding"));
            }
        }
        args.items.sort_by(|a, b| a.entity.cmp(&b.entity));
        if args.items.windows(2).any(|w| w[0].entity == w[1].entity) {
            return Err(invalid("duplicate query entity"));
        }
        // Authorization for ALL entities completes before any version read.
        for item in &args.items {
            access.require(Dataset::Daily, Some(&item.entity))?;
        }
        Ok(Self { items: args.items })
    }
}

/// Per-item resolution the repository produces inside its single snapshot.
/// `unavailable` never fabricates an empty year set.
pub enum PresenceOutcome {
    Resolved {
        version_ref: VersionRef,
        years: Vec<i32>,
    },
    Unavailable(YearPresenceReason),
}

/// Repository port: resolves the whole plan inside ONE read transaction.
pub trait PresenceRepository {
    fn year_presence(
        &self,
        plan: &PresencePlan,
        access: &Access,
    ) -> Result<Vec<PresenceOutcome>, Error>;
}

pub fn year_presence<R: PresenceRepository>(
    repository: &R,
    snapshot_id: String,
    args: YearPresenceQuery,
    access: &Access,
) -> Result<YearPresenceReply, Error> {
    let plan = PresencePlan::new(args, access)?;
    let outcomes = crate::stage_trace::timed("presence.repository", || {
        repository.year_presence(&plan, access)
    })?;
    if outcomes.len() != plan.items.len() {
        return Err(Error::Integrity(
            "presence resolution count mismatch".into(),
        ));
    }
    let items = plan
        .items
        .iter()
        .zip(outcomes)
        .map(|(item, outcome)| {
            let (status, version_ref, present_years, reason_code) = match outcome {
                PresenceOutcome::Resolved { version_ref, years } => (
                    YearPresenceStatus::Resolved,
                    Some(version_ref),
                    Some(years),
                    None,
                ),
                PresenceOutcome::Unavailable(reason) => {
                    (YearPresenceStatus::Unavailable, None, None, Some(reason))
                }
            };
            YearPresenceResult {
                entity: item.entity.clone(),
                base_daily_version_id: item.base_daily_version_id.clone(),
                status,
                version_ref,
                present_years,
                reason_code,
            }
        })
        .collect::<Vec<_>>();
    let reply = YearPresenceReply {
        snapshot_id,
        snapshot_time: Utc::now(),
        items,
        page_complete: true,
    };
    let wire_len = crate::stage_trace::timed("presence.budget", || {
        serde_json::to_vec(&reply).map_err(|e| Error::Storage(e.to_string()))
    })?
    .len();
    if wire_len > MAX_BYTES {
        return Err(invalid("snapshot budget exceeded"));
    }
    Ok(reply)
}
