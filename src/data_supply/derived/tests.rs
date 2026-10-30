//! Derived daily-run item list tests (issue-383 §9.2, M1 PR-7): identity,
//! order, generation binding and scope ownership. The end-to-end run lives in
//! `tests/mcp/plan_run.rs` (worker + real source fixture).
use super::*;

fn resolution() -> crate::data_supply::delisted_series::Resolution {
    crate::data_supply::delisted_series::Resolution {
        baseline: crate::data_supply::delisted_series::Baseline {
            as_of: chrono::NaiveDate::from_ymd_opt(2026, 10, 28).unwrap(),
            registrations: Vec::new(),
        },
        skipped: Vec::new(),
    }
}

#[test]
fn item_specs_cover_nine_indices_then_the_calendar() {
    let generation = json!("generation-1");
    let resolution = resolution();
    let items = item_specs(&generation, Some(&resolution));
    assert_eq!(
        items.len(),
        index_registry::INDEX_SERIES.len() + 1 + task_control::derived::DATASETS.len() + 1 + 1
    );
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
    let calendar = &items[index_registry::INDEX_SERIES.len()];
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
    // The M2 datasets follow the calendar, main table first: the suspension
    // gaps are derived from the master this same run writes (§12.1).
    let m2 = &items[index_registry::INDEX_SERIES.len() + 1
        ..index_registry::INDEX_SERIES.len() + 1 + task_control::derived::DATASETS.len()];
    let ids: Vec<&str> = m2.iter().map(|item| item.item_id.as_str()).collect();
    assert_eq!(ids, task_control::derived::DATASETS);
    for item in m2 {
        assert_eq!(item.target, task_control::derived::target(&item.item_id));
        assert_eq!(item.progress_unit, PROGRESS_UNIT);
        assert_eq!(
            item.scope_keys,
            BTreeSet::from([task_control::derived::dataset_scope(&item.item_id)])
        );
        assert_eq!(item.frozen_inputs[SOURCE_GENERATION_KEY], generation);
    }
    // The M3 industry item runs last and owns its own scope: it writes the M3
    // index tables instead of an M2 dataset version (M3 PR-6, `#446` §10.1).
    let industry =
        &items[index_registry::INDEX_SERIES.len() + 1 + task_control::derived::DATASETS.len()];
    assert_eq!(industry.item_id, task_control::derived::INDUSTRY_SNAPSHOT);
    assert_eq!(
        industry.target,
        task_control::derived::target(&industry.item_id)
    );
    assert_eq!(industry.progress_unit, PROGRESS_UNIT);
    assert_eq!(
        industry.scope_keys,
        BTreeSet::from([task_control::derived::industry_scope()])
    );
    assert_eq!(industry.frozen_inputs[SOURCE_GENERATION_KEY], generation);
    // The M3 delisted-series check runs after every writer: it must observe the
    // state this run's entity items left behind (M3 PR-5, `#446` §4.5).
    let check =
        &items[index_registry::INDEX_SERIES.len() + 1 + task_control::derived::DATASETS.len() + 1];
    assert_eq!(check.item_id, task_control::derived::DELISTED_SERIES_CHECK);
    assert_eq!(check.target, task_control::derived::target(&check.item_id));
    assert_eq!(check.progress_unit, PROGRESS_UNIT);
    assert_eq!(
        check.scope_keys,
        BTreeSet::from([task_control::derived::delisted_scope()])
    );
    assert_eq!(check.frozen_inputs[SOURCE_GENERATION_KEY], generation);
    assert!(
        check
            .frozen_inputs
            .get(crate::data_supply::delisted_series::BASELINE_KEY)
            .is_some()
    );
}

#[test]
fn a_backend_without_m3_tables_has_no_delisting_check_item() {
    // Legacy schema 18 keeps neither the main table nor series facts, so the
    // item would be a permanent no-op; it is absent from the set instead.
    let items = item_specs(&json!("generation-1"), None);
    assert!(
        !items
            .iter()
            .any(|item| item.item_id == task_control::derived::DELISTED_SERIES_CHECK)
    );
}

#[test]
fn collapsed_base_duplicates_keep_their_audit_shape() {
    // The main table's evidence must name the market, code and file record
    // indices of every byte-identical repeat the parser folded (XDM-70).
    let duplicates = [crate::official_security::CollapsedDuplicate {
        market: 0,
        code: "000020".into(),
        records: vec![19, 20],
    }];
    assert_eq!(
        collapsed_duplicates_json(&duplicates),
        vec![json!({"market": 0, "code": "000020", "records": [19, 20]})]
    );
    assert!(collapsed_duplicates_json(&[]).is_empty());
}

#[test]
fn item_ids_are_unique_and_never_collide_with_security_items() {
    let resolution = resolution();
    let items = item_specs(&Value::Null, Some(&resolution));
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
