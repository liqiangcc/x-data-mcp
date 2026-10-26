//! Derived daily-run item list tests (issue-383 §9.2, M1 PR-7): identity,
//! order, generation binding and scope ownership. The end-to-end run lives in
//! `tests/mcp/plan_run.rs` (worker + real source fixture).
use super::*;

#[test]
fn item_specs_cover_nine_indices_then_the_calendar() {
    let generation = json!("generation-1");
    let items = item_specs(&generation);
    assert_eq!(items.len(), index_registry::INDEX_SERIES.len() + 1);
    for (item, registration) in items.iter().zip(index_registry::INDEX_SERIES.iter()) {
        assert_eq!(
            item.item_id,
            task_control::derived::index(registration.entity)
        );
        assert_eq!(item.target, task_control::derived::target(&item.item_id));
        assert_eq!(item.target["dataset"], json!(item.item_id));
        assert_eq!(item.progress_unit, PROGRESS_UNIT);
        assert_eq!(
            item.scope_keys,
            BTreeSet::from([task_control::derived::index_scope(registration.entity)])
        );
        assert_eq!(item.frozen_inputs[SOURCE_GENERATION_KEY], generation);
    }
    let calendar = items.last().unwrap();
    assert_eq!(calendar.item_id, task_control::derived::CALENDAR);
    assert_eq!(
        calendar.target,
        task_control::derived::target(&calendar.item_id)
    );
    assert_eq!(
        calendar.scope_keys,
        BTreeSet::from([task_control::derived::calendar_scope()])
    );
    assert_eq!(calendar.frozen_inputs[SOURCE_GENERATION_KEY], generation);
}

#[test]
fn item_ids_are_unique_and_never_collide_with_security_items() {
    let items = item_specs(&Value::Null);
    let ids: BTreeSet<&str> = items.iter().map(|i| i.item_id.as_str()).collect();
    assert_eq!(ids.len(), items.len());
    let security = crate::task_control::legacy::scope_id("sh.600006", "day");
    assert!(!ids.contains(security.as_str()));
    // The env-mode (no generation) binding stays uniform: the key is present
    // and null on every item, which `uniform_binding` reads as "unbound"
    // rather than "missing" for the whole task.
    assert!(
        items
            .iter()
            .all(|i| i.frozen_inputs.get(SOURCE_GENERATION_KEY) == Some(&Value::Null))
    );
}
