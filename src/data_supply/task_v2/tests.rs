use super::*;
use crate::data_supply::wire::*;
use chrono::{DateTime, Utc};
use serde_json::json;

fn at(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .unwrap()
        .with_timezone(&Utc)
}

fn summary() -> TaskSummary {
    TaskSummary {
        task_id: "task-1".into(),
        request_id: "request-1".into(),
        dataset: "market.daily".into(),
        operation: "update_data".into(),
        contract_version: "market-plan-run-task-v1".into(),
        status: TaskStatus::PartiallyFailed,
        state_revision: 7,
        created_at: at("2026-10-26T10:00:00Z"),
        last_progress_at: at("2026-10-26T10:05:00Z"),
        plan_id: Some("plan-1".into()),
        plan_counts: None,
    }
}

fn counts(failed: usize) -> TaskCounts {
    TaskCounts {
        total: 150,
        pending: 0,
        running: 0,
        finished: 150,
        succeeded: 150 - failed,
        failed,
    }
}

fn item(item_id: &str, success: Option<bool>, error: Option<&str>) -> ProgressItem {
    ProgressItem {
        item_id: item_id.into(),
        entity: None,
        period: None,
        progress_unit: "security_period".into(),
        stage: if success.is_some() {
            "finished".into()
        } else {
            "pending".into()
        },
        success,
        error: error.map(str::to_owned),
        reason_code: None,
    }
}

fn page(unfinished: Vec<ProgressItem>) -> ProgressPage {
    ProgressPage {
        snapshot_id: "snapshot-1".into(),
        snapshot_time: at("2026-10-26T10:06:00Z"),
        task: summary(),
        total: 2,
        finished: 1,
        succeeded: 0,
        failed: 1,
        pending: 1,
        running: 0,
        execution_percent: "50.00".into(),
        unfinished,
        next_cursor: None,
        page_complete: true,
        plan: None,
        publication_versions: vec![],
        frozen_daily_ranges: vec![],
        range_results: vec![],
        frozen_yearly_inputs: vec![],
        yearly_results: vec![],
        stopped_write_check: None,
        publication_check: None,
    }
}

#[test]
fn item_ids_round_trip_to_the_string_retry_task_accepts() {
    for text in [
        "[\"market\",\"sh.600000\",\"day\"]",
        "[\"market\",\"sz.000001\",\"year\"]",
        "derived:calendar",
        "derived:index:sh.000001",
        "unexpected-shape",
    ] {
        assert_eq!(item_id(text).text(), text, "{text}");
    }
    assert_eq!(
        item_id("[\"market\",\"sh.600000\",\"day\"]"),
        ItemId {
            dataset: "market".into(),
            entity: Some("sh.600000".into()),
            period: Some("day".into()),
        }
    );
    // The two derived items stay distinguishable and carry no entity/period.
    assert_eq!(
        item_id("derived:index:sh.000001"),
        ItemId {
            dataset: "derived:index:sh.000001".into(),
            entity: None,
            period: None,
        }
    );
    let derived = serde_json::to_value(item_id("derived:calendar")).unwrap();
    assert_eq!(derived, json!({"dataset":"derived:calendar"}));
}

#[test]
fn errors_are_classified_and_bounded() {
    let error = format!("RevisionUnverified([{}])", "2003-03-25, ".repeat(600));
    let bounded = item_error(&item(
        "[\"market\",\"sh.700010\",\"day\"]",
        Some(false),
        Some(error.as_str()),
    ));
    assert_eq!(bounded.reason_code, "REVISION_UNVERIFIED");
    let text = bounded.error.unwrap();
    assert!(text.chars().count() < 300, "{}", text.chars().count());
    assert!(text.ends_with("chars total)"), "{text}");
    assert_eq!(
        bounded.item_id,
        ItemId {
            dataset: "market".into(),
            entity: Some("sh.700010".into()),
            period: Some("day".into()),
        }
    );
}

#[test]
fn a_classified_item_is_not_bounded_twice() {
    // Large-task keyset pages already bound the error and set the class; the
    // v2 projection must keep both verbatim.
    let mut item = item(
        "[\"market\",\"sh.700010\",\"day\"]",
        Some(false),
        Some("head… (94 chars total)"),
    );
    item.reason_code = Some("REVISION_UNVERIFIED".into());
    let projected = item_error(&item);
    assert_eq!(projected.error.as_deref(), Some("head… (94 chars total)"));
    assert_eq!(projected.reason_code, "REVISION_UNVERIFIED");
}

#[test]
fn reason_codes_are_the_advertised_set() {
    for error in [
        "RevisionUnverified([...])",
        "PUBLISH_PRECONDITION: base moved",
        "no bars inside the accepted range",
        "something else",
        "",
    ] {
        assert!(
            REASON_CODES.contains(&reason_code(error)),
            "{error} -> {}",
            reason_code(error)
        );
    }
}

#[test]
fn failed_by_reason_counts_only_failed_items() {
    let items = vec![
        item("i1", Some(false), Some("RevisionUnverified([])")),
        item("i2", Some(false), Some("RevisionUnverified([])")),
        item("i3", Some(false), Some("no bars inside the accepted range")),
        item("i4", Some(true), None),
        item("i5", None, None),
    ];
    let counts = failed_by_reason(&items);
    assert_eq!(counts.get("REVISION_UNVERIFIED"), Some(&2));
    assert_eq!(counts.get("NO_BARS_IN_RANGE"), Some(&1));
    assert_eq!(counts.len(), 2);
}

#[test]
fn task_view_omits_the_diagnosis_when_nothing_failed() {
    let value = task_view(summary(), counts(0), BTreeMap::new(), &[]);
    assert_eq!(value["wire_contract_version"], "v2");
    assert_eq!(value["task_contract_version"], "market-plan-run-task-v1");
    assert!(value.get("contract_version").is_none(), "{value}");
    assert!(value.get("failed_by_reason").is_none(), "{value}");
    assert!(value.get("errors").is_none(), "{value}");
    assert_eq!(value["failed"], 0);
    assert_eq!(value["succeeded"], 150);
}

#[test]
fn task_view_samples_at_most_twenty_errors() {
    let items: Vec<ProgressItem> = (0..25)
        .map(|i| {
            item(
                &format!("[\"market\",\"sh.{:06}\",\"day\"]", 700_000 + i),
                Some(false),
                Some("RevisionUnverified([])"),
            )
        })
        .collect();
    let mut by_reason = BTreeMap::new();
    by_reason.insert("REVISION_UNVERIFIED".to_string(), 25);
    let value = task_view(summary(), counts(25), by_reason, &items[..ERROR_SAMPLE]);
    assert_eq!(value["errors"].as_array().unwrap().len(), ERROR_SAMPLE);
    assert_eq!(value["failed_by_reason"]["REVISION_UNVERIFIED"], 25);
}

#[test]
fn progress_view_structures_item_ids_and_moves_the_classification() {
    let mut page = page(vec![
        item(
            "[\"market\",\"sh.700010\",\"day\"]",
            Some(false),
            Some("RevisionUnverified([])"),
        ),
        item("derived:calendar", None, None),
    ]);
    page.plan = Some(PlanProgress {
        plan_id: "plan-1".into(),
        generation_id: Some("gen-1".into()),
        target_day: Some(chrono::NaiveDate::from_ymd_opt(2026, 10, 24).unwrap()),
        failed_by_reason: BTreeMap::from([("REVISION_UNVERIFIED".to_string(), 1)]),
    });
    let mut by_reason = BTreeMap::new();
    by_reason.insert("REVISION_UNVERIFIED".to_string(), 1);
    let value = progress_view(page, by_reason).unwrap();
    assert_eq!(value["wire_contract_version"], "v2");
    assert_eq!(
        value["task"]["task_contract_version"],
        "market-plan-run-task-v1"
    );
    assert_eq!(value["failed_by_reason"]["REVISION_UNVERIFIED"], 1);
    assert!(
        value["plan"].get("failed_by_reason").is_none(),
        "the classification must not be repeated: {value}"
    );
    assert_eq!(value["plan"]["plan_id"], "plan-1");
    assert_eq!(value["unfinished"][0]["item_id"]["entity"], "sh.700010");
    assert_eq!(value["unfinished"][0]["reason_code"], "REVISION_UNVERIFIED");
    assert_eq!(
        value["unfinished"][1]["item_id"]["dataset"],
        "derived:calendar"
    );
    assert!(value["unfinished"][1]["item_id"].get("entity").is_none());
    assert!(value["unfinished"][1].get("reason_code").is_none());
}

#[test]
fn progress_view_structures_every_item_reference() {
    let version = VersionRef {
        dataset: Dataset::Daily,
        contract_version: ContractVersion::V1,
        entity: Some(Security {
            exchange: Exchange::Sh,
            code: "600000".into(),
        }),
        version_id: "v1".into(),
        integrity_sha256: "v1".into(),
    };
    let mut page = page(vec![]);
    page.frozen_daily_ranges = vec![FrozenDailyRange {
        item_id: "[\"market\",\"sh.600000\",\"day\"]".into(),
        entity: Security {
            exchange: Exchange::Sh,
            code: "600000".into(),
        },
        base_version_id: "base".into(),
        base_integrity_sha256: "digest".into(),
        start_date: chrono::NaiveDate::from_ymd_opt(2026, 10, 20).unwrap(),
        end_date: chrono::NaiveDate::from_ymd_opt(2026, 10, 24).unwrap(),
    }];
    page.range_results = vec![RangeExecutionResult {
        result_id: "result".into(),
        item_id: "[\"market\",\"sh.600000\",\"day\"]".into(),
        base_version: version.clone(),
        result_version: version.clone(),
        no_change: false,
        acquired_at: at("2026-10-26T10:05:00Z"),
    }];
    page.frozen_yearly_inputs = vec![FrozenYearlyInput {
        item_id: "[\"market\",\"sh.600000\",\"year\"]".into(),
        entity: Security {
            exchange: Exchange::Sh,
            code: "600000".into(),
        },
        base_daily_version_id: "base".into(),
        base_integrity_sha256: "digest".into(),
        years: vec![2025],
        publication: YearlyPublication::HistoricalFixedBase {
            comparison_version_id: None,
        },
        comparison: None,
    }];
    page.yearly_results = vec![YearlyExecutionResult {
        result_id: "result".into(),
        item_id: "[\"market\",\"sh.600000\",\"year\"]".into(),
        base_daily_version: version.clone(),
        result_version: version,
        no_change: true,
        derived_at: at("2026-10-26T10:05:00Z"),
    }];
    let value = progress_view(page, BTreeMap::new()).unwrap();
    for (array, entity) in [
        ("frozen_daily_ranges", "sh.600000"),
        ("range_results", "sh.600000"),
        ("frozen_yearly_inputs", "sh.600000"),
        ("yearly_results", "sh.600000"),
    ] {
        assert_eq!(
            value[array][0]["item_id"]["entity"], entity,
            "{array}: {value}"
        );
        assert!(
            value[array][0].get("entity").is_none(),
            "{array} must not repeat the entity: {value}"
        );
    }
    assert!(value.get("failed_by_reason").is_none(), "{value}");
}
