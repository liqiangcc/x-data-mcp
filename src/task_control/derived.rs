//! Server-internal derived item identities (issue-383 §9.2, M1 PR-7).
//!
//! The daily run maintains five data classes in one task. Four of them are
//! per-entity items (`market.daily` rows); the calendar and the benchmark
//! indices are *derived datasets* whose write covers the whole market at
//! once, so they cannot be an entity item. They are still ordinary
//! `task_items` rows — the same counters, progress projection and failure
//! semantics apply — and only their identity differs:
//!
//! - `item_id` starts with [`PREFIX`] and names the dataset it maintains
//!   (`derived:calendar`, `derived:index:sh.000001`);
//! - `target_json` carries that same dataset ([`target`]) instead of the
//!   entity/period pair a security item uses, so `get_task_progress` reports
//!   `entity`/`period` as `null` for it (`ProgressItem` fields are optional;
//!   the wire shape is unchanged);
//! - the series a derived item writes is still bound by its `scope_keys`
//!   occupancy key, so the storage commit gate can tell an index item from a
//!   stock item without a second registry.
//!
//! No wire type, tool or dataset is added: these items are created by the
//! server inside a plan-run task and are only observable through the existing
//! task/progress surface.

use serde_json::{Value, json};

use super::legacy;

/// Prefix of every server-created derived item.
pub const PREFIX: &str = "derived:";
/// The one trading-calendar item (§9.2).
pub const CALENDAR: &str = "derived:calendar";
/// The M2 whole-market datasets maintained by the same daily run (issue-391
/// §12.1). Each is one item because one write covers the whole market.
pub const SECURITY_MASTER: &str = "derived:security_master";
pub const RISK_WARNINGS: &str = "derived:risk_warnings";
pub const SUSPENSIONS: &str = "derived:suspensions";
pub const TRADING_RULES: &str = "derived:trading_rules";
/// The M3 historical-industry item (M3 PR-6, `#446` §10.1): one `tdxhy.cfg`
/// snapshot plus its intervals and dictionary per run. It is not an M2
/// `DATASETS` entry: its write set is the M3 index tables, not a
/// `derived_dataset_versions` registration (§5.1).
pub const INDUSTRY_SNAPSHOT: &str = "derived:industry_snapshot";
/// The M3 delisted-series check (M3 PR-5, `#446` §4.5/§10.1). It is not an M2
/// `DATASETS` entry either, and unlike every other derived item it writes **no**
/// business table: it compares terminal series against the registration frozen
/// at run start and reports, so it owns a read-only scope.
pub const DELISTED_SERIES_CHECK: &str = "derived:delisted_series_check";
/// Every M2 dataset item, in execution order: the main table first (the
/// suspension gaps are derived from it), then the datasets whose source is
/// still pending, then the rules (admitted by D-44; the item writes the
/// versioned rule book and reports `no_change` when it is already stored).
pub const DATASETS: [&str; 4] = [SECURITY_MASTER, RISK_WARNINGS, SUSPENSIONS, TRADING_RULES];
/// Prefix of the per-benchmark index items (§9.2); the suffix is the entity.
pub const INDEX_PREFIX: &str = "derived:index:";

/// Item id of one benchmark index's derived item.
pub fn index(entity: &str) -> String {
    format!("{INDEX_PREFIX}{entity}")
}

/// The entity an index derived item maintains, `None` for any other id.
pub fn index_entity(item_id: &str) -> Option<&str> {
    item_id.strip_prefix(INDEX_PREFIX)
}

/// `true` for a server-created derived item id.
pub fn is_derived(item_id: &str) -> bool {
    item_id.starts_with(PREFIX)
}

/// The `target_json` of a derived item: the dataset it maintains, named by
/// its own item id. The item identity and the target cannot drift apart.
pub fn target(item_id: &str) -> Value {
    json!({"dataset": item_id})
}

/// Occupancy key of the calendar item. The calendar owns no security series,
/// so it declares its own scope instead of borrowing an entity's.
pub fn calendar_scope() -> String {
    json!(["market", CALENDAR, "calendar"]).to_string()
}

/// Occupancy key of one M2 whole-market dataset item. It owns no security
/// series, so it declares its own scope instead of borrowing an entity's (the
/// same rule the calendar item follows).
pub fn dataset_scope(item_id: &str) -> String {
    json!(["m2", item_id, "market"]).to_string()
}

/// Occupancy key of the M3 industry item. It owns no security series either,
/// so it declares its own scope; the key names the M3 table set it writes.
pub fn industry_scope() -> String {
    json!(["m3", INDUSTRY_SNAPSHOT, "market"]).to_string()
}

/// Occupancy key of the M3 delisted-series check. The item writes nothing, so
/// its scope declares no series; it is its own key so the occupancy gate sees
/// the item as its own unit instead of borrowing an entity's series.
pub fn delisted_scope() -> String {
    json!(["m3", DELISTED_SERIES_CHECK, "market"]).to_string()
}

/// Occupancy key of one benchmark index item — the same key the index series
/// commit checks (`Scope { symbol, period: Day }`), so a derived index item
/// can only write the series it declares.
pub fn index_scope(entity: &str) -> String {
    legacy::scope_id(entity, "day")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn derived_identity_round_trips() {
        assert!(is_derived(CALENDAR));
        assert_eq!(index_entity(CALENDAR), None);
        let id = index("sh.000001");
        assert_eq!(id, "derived:index:sh.000001");
        assert!(is_derived(&id));
        assert_eq!(index_entity(&id), Some("sh.000001"));
        // A security item id (the scope-id JSON) is never a derived item.
        assert!(!is_derived(&legacy::scope_id("sh.600006", "day")));
        assert_eq!(index_entity("sh.000001"), None);
    }

    #[test]
    fn target_names_the_item_dataset() {
        assert_eq!(target(CALENDAR), json!({"dataset": CALENDAR}));
        assert_eq!(
            target(&index("sz.399001")),
            json!({"dataset": "derived:index:sz.399001"})
        );
        // The index occupancy key uses the same `["market", entity, "day"]`
        // shape as a security scope, so the series commit's occupancy check
        // binds a derived index item to the index it declares.
        assert_eq!(index_scope("sz.399001"), r#"["market","sz.399001","day"]"#);
        assert_ne!(calendar_scope(), index_scope("sh.000001"));
    }

    #[test]
    fn m2_dataset_items_own_distinct_whole_market_scopes() {
        assert!(is_derived(SECURITY_MASTER));
        assert_eq!(DATASETS.len(), 4);
        let scopes: BTreeSet<String> = DATASETS.iter().map(|id| dataset_scope(id)).collect();
        assert_eq!(scopes.len(), DATASETS.len());
        assert!(!scopes.contains(&calendar_scope()));
        assert_eq!(target(SUSPENSIONS), json!({"dataset": SUSPENSIONS}));
    }

    #[test]
    fn industry_item_is_derived_and_owns_its_own_scope() {
        assert!(is_derived(INDUSTRY_SNAPSHOT));
        // Not an M2 `DATASETS` entry: the M2 list gates `derived_dataset_versions`
        // writers, and the industry tables are written by a different path.
        assert!(!DATASETS.contains(&INDUSTRY_SNAPSHOT));
        assert_eq!(
            target(INDUSTRY_SNAPSHOT),
            json!({"dataset": INDUSTRY_SNAPSHOT})
        );
        let scopes: BTreeSet<String> = DATASETS
            .iter()
            .map(|id| dataset_scope(id))
            .chain([industry_scope()])
            .collect();
        assert_eq!(scopes.len(), DATASETS.len() + 1);
        assert_ne!(industry_scope(), calendar_scope());
    }

    #[test]
    fn delisted_check_is_a_read_only_derived_item() {
        assert!(is_derived(DELISTED_SERIES_CHECK));
        // No `derived_dataset_versions` row is written by this item, so it must
        // stay out of the M2 writer list.
        assert!(!DATASETS.contains(&DELISTED_SERIES_CHECK));
        assert_eq!(
            target(DELISTED_SERIES_CHECK),
            json!({"dataset": DELISTED_SERIES_CHECK})
        );
        assert_ne!(delisted_scope(), industry_scope());
        assert_ne!(delisted_scope(), calendar_scope());
        assert_ne!(delisted_scope(), index_scope("sh.000001"));
    }
}
