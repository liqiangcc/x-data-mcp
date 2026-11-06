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

// ---------------------------------------------------------------------------
// M3-2 PR-A: ETF failure is local to the ETF part of the main table.
// ---------------------------------------------------------------------------

fn a_share_assembly() -> assembly::Assembly {
    use crate::security_master::{
        DelistingReason, DelistingSource, Exchange, IssuerResolution, ListingSource,
        SecurityLifecycleFact,
    };
    let tdx = vec![SecurityLifecycleFact {
        exchange: Exchange::Sh,
        code: "600000".into(),
        effective_from: chrono::NaiveDate::from_ymd_opt(1999, 11, 10).unwrap(),
        effective_to: None,
        instrument_class: 0,
        issuer_key: None,
        issuer_resolution: IssuerResolution::CodeDate,
        listing_date: chrono::NaiveDate::from_ymd_opt(1999, 11, 10),
        listing_date_tdx: chrono::NaiveDate::from_ymd_opt(1999, 11, 10),
        listing_source: ListingSource::TdxSsdate,
        b_listing_date: None,
        delisting_date: None,
        delisting_date_tdx: None,
        delisting_source: DelistingSource::Unknown,
        delisting_reason_class: DelistingReason::Unknown,
        arrangement_start: None,
        arrangement_end: None,
        listing_conflict: false,
        delisting_conflict: false,
        known_at: None,
    }];
    assembly::assemble_security_master(
        &tdx,
        &[],
        assembly::ExchangeSource::Pending(PendingSource {
            dataset: assembly::DATASET_KIND,
            source: "exchange_list",
            reason: "test",
        }),
        &[],
    )
    .unwrap()
}

fn etf_member(exchange: crate::security_master::Exchange, code: &str) -> assembly::MemberFact {
    assembly::MemberFact {
        exchange,
        code: code.into(),
        first_seen: chrono::NaiveDate::from_ymd_opt(2026, 11, 3).unwrap(),
        known_at: chrono::NaiveDate::from_ymd_opt(2026, 11, 3).unwrap(),
        listing_date: Some(chrono::NaiveDate::from_ymd_opt(2005, 2, 23).unwrap()),
        listing_source: crate::security_master::ListingSource::ExchangeList,
        listing_date_tdx: Some(chrono::NaiveDate::from_ymd_opt(2005, 2, 23).unwrap()),
        effective_to: None,
        delisting_date: None,
        delisting_source: crate::security_master::DelistingSource::Unknown,
        delisting_reason_class: crate::security_master::DelistingReason::Unknown,
        announcement_date: None,
        announcement_url: None,
        extraction: None,
    }
}

fn class_zero(
    rows: &[crate::security_master::SecurityLifecycleFact],
) -> Vec<crate::security_master::SecurityLifecycleFact> {
    rows.iter()
        .filter(|row| row.instrument_class == 0)
        .cloned()
        .collect()
}

/// D-80: the listing day opens an ETF row's interval and the A-share rows of
/// the same assembly are exactly what they were without any ETF.
#[test]
fn etf_rows_start_at_the_listing_day_and_a_share_rows_are_unchanged() {
    let alone = a_share_assembly();
    let before = alone.rows.clone();
    let mut assembled = a_share_assembly();
    let facts = vec![etf_member(crate::security_master::Exchange::Sh, "510050")];
    let report = merge_etf_identity(
        &mut assembled,
        Vec::new(),
        EtfAttempt::Facts {
            facts: Ok(facts),
            evidence: json!({}),
            failure: None,
            preserve_previous: false,
        },
    );
    assert!(report.failure.is_none());
    assert_eq!(class_zero(&assembled.rows), before);
    // The A-share block is byte-identical, so is its digest.
    assert_eq!(
        assembly::assembly_digest(&class_zero(&assembled.rows), &[]),
        assembly::assembly_digest(&before, &[])
    );
    let etf: Vec<_> = assembled
        .rows
        .iter()
        .filter(|row| row.instrument_class == 2)
        .collect();
    assert_eq!(etf.len(), 1);
    // Listed 2005-02-23, first observed 2026-11-03.
    assert_eq!(
        etf[0].effective_from,
        chrono::NaiveDate::from_ymd_opt(2005, 2, 23).unwrap()
    );
    assert_eq!(
        etf[0].known_at,
        chrono::NaiveDate::from_ymd_opt(2026, 11, 3)
    );
}

#[test]
fn an_etf_failure_keeps_the_previous_class_two_rows_and_still_writes_a_shares() {
    let mut assembled = a_share_assembly();
    let before = class_zero(&assembled.rows);
    let previous =
        assembly::member_rows(&[etf_member(crate::security_master::Exchange::Sh, "510050")])
            .unwrap();
    let report = merge_etf_identity(
        &mut assembled,
        previous.clone(),
        EtfAttempt::Failed {
            code: "source_invalid".into(),
            detail: "the xlsx shape changed".into(),
        },
    );
    assert_eq!(report.failure.as_ref().unwrap().0, "source_invalid");
    assert_eq!(report.evidence["status"], "failed");
    assert_eq!(report.evidence["preserved_rows"], 1);
    assert_eq!(class_zero(&assembled.rows), before);
    let class_two: Vec<_> = assembled
        .rows
        .iter()
        .filter(|row| row.instrument_class == 2)
        .cloned()
        .collect();
    assert_eq!(class_two, previous);
}

#[test]
fn an_etf_fact_or_member_error_is_a_partial_failure_and_keeps_previous_rows() {
    let mut assembled = a_share_assembly();
    let before = class_zero(&assembled.rows);
    let previous =
        assembly::member_rows(&[etf_member(crate::security_master::Exchange::Sz, "159901")])
            .unwrap();
    let report = merge_etf_identity(
        &mut assembled,
        previous.clone(),
        EtfAttempt::Facts {
            facts: Err((
                "member_listing_conflict".into(),
                "listing day differs".into(),
            )),
            evidence: json!({"parser_version": "etf-list-v1"}),
            failure: None,
            preserve_previous: false,
        },
    );
    assert_eq!(
        report.failure.as_ref().unwrap().0,
        "member_listing_conflict"
    );
    assert_eq!(report.evidence["status"], "failed");
    assert_eq!(report.evidence["preserved_rows"], 1);
    assert_eq!(class_zero(&assembled.rows), before);
    let class_two: Vec<_> = assembled
        .rows
        .iter()
        .filter(|row| row.instrument_class == 2)
        .cloned()
        .collect();
    assert_eq!(class_two, previous);
}

#[test]
fn an_etf_pending_source_keeps_previous_rows_without_failing_the_item() {
    let mut assembled = a_share_assembly();
    let before = class_zero(&assembled.rows);
    let previous =
        assembly::member_rows(&[etf_member(crate::security_master::Exchange::Sh, "510050")])
            .unwrap();
    let report = merge_etf_identity(
        &mut assembled,
        previous.clone(),
        EtfAttempt::Pending(PendingSource {
            dataset: assembly::DATASET_KIND,
            source: "etf_list",
            reason: "no frozen list",
        }),
    );
    assert!(report.failure.is_none());
    assert_eq!(report.evidence["status"], "pending");
    assert_eq!(report.evidence["preserved_rows"], 1);
    assert_eq!(class_zero(&assembled.rows), before);
    let class_two: Vec<_> = assembled
        .rows
        .iter()
        .filter(|row| row.instrument_class == 2)
        .cloned()
        .collect();
    assert_eq!(class_two, previous);
}

#[test]
fn a_successful_etf_attempt_replaces_previous_rows_and_keeps_a_shares() {
    let mut assembled = a_share_assembly();
    let before = class_zero(&assembled.rows);
    let previous =
        assembly::member_rows(&[etf_member(crate::security_master::Exchange::Sh, "510050")])
            .unwrap();
    let new = vec![
        etf_member(crate::security_master::Exchange::Sh, "510050"),
        etf_member(crate::security_master::Exchange::Sz, "159901"),
    ];
    let report = merge_etf_identity(
        &mut assembled,
        previous,
        EtfAttempt::Facts {
            facts: Ok(new),
            evidence: json!({"parser_version": "etf-list-v1"}),
            failure: None,
            preserve_previous: false,
        },
    );
    assert!(report.failure.is_none());
    assert_eq!(report.evidence["status"], "ok");
    assert_eq!(report.evidence["member_count"], 2);
    assert_eq!(class_zero(&assembled.rows), before);
    assert_eq!(
        assembled
            .rows
            .iter()
            .filter(|row| row.instrument_class == 2)
            .count(),
        2
    );
}

#[test]
fn an_etf_last_bar_failure_is_an_etf_only_failure_that_preserves_a_shares_and_delisted_rows() {
    let mut assembled = a_share_assembly();
    let before = class_zero(&assembled.rows);
    let previous = assembly::member_rows(&[
        etf_member(crate::security_master::Exchange::Sh, "510050"),
        assembly::MemberFact {
            exchange: crate::security_master::Exchange::Sh,
            code: "512390".into(),
            first_seen: chrono::NaiveDate::from_ymd_opt(2026, 1, 2).unwrap(),
            known_at: chrono::NaiveDate::from_ymd_opt(2026, 1, 2).unwrap(),
            listing_date: Some(chrono::NaiveDate::from_ymd_opt(2010, 1, 1).unwrap()),
            listing_source: crate::security_master::ListingSource::TdxSsdate,
            listing_date_tdx: Some(chrono::NaiveDate::from_ymd_opt(2010, 1, 1).unwrap()),
            effective_to: Some(chrono::NaiveDate::from_ymd_opt(2026, 8, 5).unwrap()),
            delisting_date: Some(chrono::NaiveDate::from_ymd_opt(2026, 8, 5).unwrap()),
            delisting_source: crate::security_master::DelistingSource::ExchangeList,
            delisting_reason_class: crate::security_master::DelistingReason::Other,
            announcement_date: Some(chrono::NaiveDate::from_ymd_opt(2026, 8, 1).unwrap()),
            announcement_url: None,
            extraction: Some("cd".repeat(32)),
        },
    ])
    .unwrap();
    let candidates = BTreeSet::from(["sh.512390".to_string()]);
    let (code, detail) = collect_last_bars(&candidates, |entity| {
        Err(format!("{entity}: synthetic read failure"))
    })
    .unwrap_err();
    assert_eq!(code, "day_last_bar_unavailable");
    let report = merge_etf_identity(
        &mut assembled,
        previous,
        EtfAttempt::Facts {
            facts: Ok(vec![etf_member(
                crate::security_master::Exchange::Sh,
                "510050",
            )]),
            evidence: json!({"current": "synthetic"}),
            failure: Some((code, detail)),
            preserve_previous: true,
        },
    );
    assert_eq!(
        report.failure.as_ref().unwrap().0,
        "day_last_bar_unavailable"
    );
    assert_eq!(report.evidence["status"], "partial");
    assert_eq!(class_zero(&assembled.rows), before);
    let class_two: Vec<_> = assembled
        .rows
        .iter()
        .filter(|row| row.instrument_class == 2)
        .map(|row| row.code.clone())
        .collect();
    assert!(class_two.contains(&"510050".to_string()));
    assert!(
        class_two.contains(&"512390".to_string()),
        "a failed delisted branch must keep the previous delisted row"
    );
}

#[test]
fn a_pending_delisted_branch_also_keeps_previous_class_two_rows() {
    let mut assembled = a_share_assembly();
    let before = class_zero(&assembled.rows);
    let previous = assembly::member_rows(&[
        etf_member(crate::security_master::Exchange::Sh, "510050"),
        assembly::MemberFact {
            exchange: crate::security_master::Exchange::Sh,
            code: "512390".into(),
            first_seen: chrono::NaiveDate::from_ymd_opt(2026, 1, 2).unwrap(),
            known_at: chrono::NaiveDate::from_ymd_opt(2026, 1, 2).unwrap(),
            listing_date: Some(chrono::NaiveDate::from_ymd_opt(2010, 1, 1).unwrap()),
            listing_source: crate::security_master::ListingSource::TdxSsdate,
            listing_date_tdx: Some(chrono::NaiveDate::from_ymd_opt(2010, 1, 1).unwrap()),
            effective_to: Some(chrono::NaiveDate::from_ymd_opt(2026, 8, 5).unwrap()),
            delisting_date: Some(chrono::NaiveDate::from_ymd_opt(2026, 8, 5).unwrap()),
            delisting_source: crate::security_master::DelistingSource::ExchangeList,
            delisting_reason_class: crate::security_master::DelistingReason::Other,
            announcement_date: Some(chrono::NaiveDate::from_ymd_opt(2026, 8, 1).unwrap()),
            announcement_url: None,
            extraction: Some("cd".repeat(32)),
        },
    ])
    .unwrap();
    let report = merge_etf_identity(
        &mut assembled,
        previous,
        EtfAttempt::Facts {
            facts: Ok(vec![etf_member(
                crate::security_master::Exchange::Sh,
                "510050",
            )]),
            evidence: json!({"current": "synthetic", "delisted": {"status": "pending"}}),
            failure: None,
            preserve_previous: true,
        },
    );
    assert!(report.failure.is_none());
    assert_eq!(class_zero(&assembled.rows), before);
    let class_two: Vec<_> = assembled
        .rows
        .iter()
        .filter(|row| row.instrument_class == 2)
        .map(|row| row.code.clone())
        .collect();
    assert!(class_two.contains(&"510050".to_string()));
    assert!(
        class_two.contains(&"512390".to_string()),
        "a pending delisted branch must not drop the previous delisted row"
    );
}
