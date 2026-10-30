//! Internal #92 default-selector resolution: catalog members intersected with
//! daily securities whose first-collection items carry a committed
//! execution-start marker, packaged as freezeable evidence. "Catalog members"
//! is the current head **plus** the members of the version published right
//! before it (M3 PR-5, `#446` §4.5), so a member that leaves the catalog cannot
//! lose the delisting day's last bars. No MCP/wire, no task creation, no gap or
//! range semantics.
//!
//! Contract-family recognition (application layer, never SQL): a marker counts
//! when its task's stored `command_key_json` is exactly the typed
//! `start_collection`/`market.daily` key (v1, or v2 for the M3 listed-ever
//! scope) or the legacy `start_collection`/`legacy_market_compound`/
//! `task-acceptance-v1` key.
//! Retry tasks already store the original executable key in
//! `tasks.command_key_json` (see `sqlite_tasks` accept), so
//! retry-of-collection markers qualify naturally; `retry_of` is evidence only.
use super::wire::{Exchange, Security};
use crate::{
    catalog::Exchange as CatalogExchange,
    hash::sha256,
    sqlite_catalog::{ExecutionStartFact, SqliteCatalog},
    task_control::CommandKey,
};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
/// Local failure boundary for the internal resolver. Nothing here is a wire
/// or task-acceptance error; the future accept path maps these itself.
#[derive(Debug, thiserror::Error)]
pub enum SelectionError {
    /// Current catalog ∩ ever-started resolved to no security.
    #[error("EMPTY_SELECTION")]
    EmptySelection,
    /// Missing/corrupt catalog, unreadable facts, or corrupt evidence rows:
    /// resolution fails closed and never returns a partial default set.
    #[error("INVALID_SELECTION_FACTS: {0}")]
    Invalid(String),
}
fn invalid(message: impl ToString) -> SelectionError {
    SelectionError::Invalid(message.to_string())
}
/// Durable identity of one execution-start marker backing a selected security.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct StartIdentity {
    pub task_id: String,
    pub item_id: String,
    pub run_id: String,
    pub start_revision: u64,
    pub started_at: String,
    /// Raw parent linkage for evidence; never used for classification.
    pub retry_of: Option<String>,
}
/// Resolved default daily securities plus everything needed to persist and
/// later re-verify the frozen selection without recomputing it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DefaultDailySelection {
    pub catalog_version_id: String,
    pub catalog_digest: String,
    /// Version whose members are unioned into the eligible set (M3 PR-5,
    /// `#446` §4.5); `None` when the head is the first published version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_catalog_version_id: Option<String>,
    /// Selected symbols that only the previous version still lists: the
    /// delisting-tail survivors this rule exists for. Sorted, usually empty.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub retained_members: Vec<String>,
    /// Sorted unique `sh.600006`-style symbols.
    pub securities: Vec<String>,
    /// Selected symbol -> every qualifying marker identity, sorted.
    pub starts: BTreeMap<String, Vec<StartIdentity>>,
    /// sha256 over the canonical serialization of the fields above.
    pub evidence_digest: String,
}
/// Marker map keyed by selected symbol; named so the literal below stays
/// one field per line for the pre-push struct-literal check.
type StartProofs = BTreeMap<String, Vec<StartIdentity>>;
#[derive(Serialize)]
struct DigestInput<'a> {
    catalog_version_id: &'a str,
    catalog_digest: &'a str,
    previous_catalog_version_id: Option<&'a str>,
    retained_members: &'a [String],
    securities: &'a [String],
    starts: &'a StartProofs,
}
fn member_symbol(record: &crate::catalog::SecurityRecord) -> Option<String> {
    let exchange = match record.exchange {
        CatalogExchange::Shanghai => Exchange::Sh,
        CatalogExchange::Shenzhen => Exchange::Sz,
        // non-SH/SZ members can never map to a ["market",symbol,period] scope
        CatalogExchange::Other(_) => return None,
    };
    Some(
        Security {
            exchange,
            code: record.symbol.clone(),
        }
        .symbol(),
    )
}
enum Family {
    Typed,
    Legacy,
}
fn family(key: &CommandKey) -> Option<Family> {
    if key.operation != "start_collection" {
        return None;
    }
    match (key.dataset.as_str(), key.contract_version.as_str()) {
        // v2 only ever collects `market.daily` (M3 #446 §4.4), so a v2 marker
        // is the same first-collection proof a v1 marker is.
        ("market.daily", "v1" | "v2") => Some(Family::Typed),
        ("legacy_market_compound", "task-acceptance-v1") => Some(Family::Legacy),
        _ => None,
    }
}
/// Daily first-collection proof for one marker row, or None when the row
/// legitimately belongs to another contract family or to a legacy year item.
/// Unreadable command keys and malformed item identities fail closed.
fn started_symbol(fact: &ExecutionStartFact) -> Result<Option<String>, SelectionError> {
    let key: CommandKey = serde_json::from_str(&fact.command_key_json)
        .map_err(|e| invalid(format!("execution-start command key unreadable: {e}")))?;
    let Some(family) = family(&key) else {
        return Ok(None);
    };
    let id: Value = serde_json::from_str(&fact.item_id)
        .map_err(|e| invalid(format!("execution-start item id unreadable: {e}")))?;
    let (ns, symbol, period) = id
        .as_array()
        .filter(|a| a.len() == 3)
        .and_then(|a| Some((a[0].as_str()?, a[1].as_str()?.to_string(), a[2].as_str()?)))
        .ok_or_else(|| invalid("execution-start item id is not a [market,symbol,period] scope"))?;
    if ns != "market" {
        return Err(invalid(
            "execution-start item identity is not a market scope",
        ));
    }
    match (family, period) {
        (Family::Typed, "day") | (Family::Legacy, "day") => Ok(Some(symbol)),
        (Family::Legacy, "year") => Ok(None),
        (Family::Typed, _) | (Family::Legacy, _) => Err(invalid(
            "execution-start item period is inconsistent with its contract family",
        )),
    }
}
/// Resolve the current default update/backfill security set: members of the
/// catalog head version that have a committed daily first-collection
/// execution-start marker. Fails closed on missing/corrupt catalog, unreadable
/// facts, corrupt rows, or an empty intersection.
pub fn resolve_default_daily(
    catalog: &SqliteCatalog,
) -> Result<DefaultDailySelection, SelectionError> {
    resolve_facts(&catalog.selection_facts().map_err(invalid)?)
}
/// Same-snapshot variant for callers that already hold a read transaction:
/// selection facts are read inside that snapshot instead of a new one.
pub(crate) fn resolve_default_daily_on(
    c: &rusqlite::Connection,
) -> Result<DefaultDailySelection, SelectionError> {
    resolve_facts(&crate::sqlite_catalog::selection_facts_on(c).map_err(invalid)?)
}
fn resolve_facts(
    facts: &crate::sqlite_catalog::SelectionFacts,
) -> Result<DefaultDailySelection, SelectionError> {
    let version_id = facts
        .catalog_head
        .clone()
        .ok_or_else(|| invalid("catalog head missing"))?;
    let version = facts
        .catalog
        .clone()
        .ok_or_else(|| invalid("catalog head version unreadable"))?;
    let catalog_digest = crate::sqlite_catalog::digest(&version).map_err(invalid)?;
    // M3 PR-5 (`#446` §4.5): the eligible set is the current catalog *plus*
    // the previous catalog version's members. A member that leaves the catalog
    // between two versions can still own bars from the delisting day, so
    // dropping it here would lose that tail; the union keeps it eligible for
    // `update_data` until its series reaches the delisting day. A member that
    // never had a first-collection marker is still not selected, and an active
    // member is selected exactly as before (the union only adds symbols).
    let mut members: BTreeSet<String> = version.records.iter().filter_map(member_symbol).collect();
    let current_members = members.clone();
    let mut previous_catalog_version_id = None;
    if let Some(previous) = &facts.previous_catalog {
        members.extend(previous.members.iter().cloned());
        previous_catalog_version_id = Some(previous.version_id.clone());
    }
    let mut starts: BTreeMap<String, Vec<StartIdentity>> = BTreeMap::new();
    for fact in &facts.starts {
        let Some(symbol) = started_symbol(fact)? else {
            continue;
        };
        if !members.contains(&symbol) {
            continue;
        }
        starts.entry(symbol).or_default().push(StartIdentity {
            task_id: fact.task_id.clone(),
            item_id: fact.item_id.clone(),
            run_id: fact.run_id.clone(),
            start_revision: fact.start_revision,
            started_at: fact.started_at.clone(),
            retry_of: fact.retry_of.clone(),
        });
    }
    for proofs in starts.values_mut() {
        proofs.sort();
    }
    let securities: Vec<String> = starts.keys().cloned().collect();
    if securities.is_empty() {
        return Err(SelectionError::EmptySelection);
    }
    // Evidence: which selected symbols exist only because of the retained
    // previous version. Usually empty (the catalog publisher refuses member
    // disappearance, `catalog::RejectReason::MemberDisappeared`), so a
    // non-empty list always names a real tail case.
    let retained_members: Vec<String> = securities
        .iter()
        .filter(|symbol| !current_members.contains(*symbol))
        .cloned()
        .collect();
    let input = DigestInput {
        catalog_version_id: &version_id,
        catalog_digest: &catalog_digest,
        previous_catalog_version_id: previous_catalog_version_id.as_deref(),
        retained_members: &retained_members,
        securities: &securities,
        starts: &starts,
    };
    let evidence_digest = sha256(
        serde_json::to_vec(&input)
            .map_err(|e| invalid(format!("selection evidence serialization: {e}")))?
            .as_slice(),
    );
    Ok(DefaultDailySelection {
        catalog_version_id: version_id,
        catalog_digest,
        previous_catalog_version_id,
        retained_members,
        securities,
        starts,
        evidence_digest,
    })
}
#[cfg(test)]
mod tests;
