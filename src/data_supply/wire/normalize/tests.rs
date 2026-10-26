//! One test per rejection in the v8 design (#387 §2.1.1 / §7.2), plus the
//! invariants the normalization layer must not break: the friendly and the
//! contract spelling decode to the same typed request (so the idempotency
//! digest is identical), normalization is idempotent, and the "still
//! rejected" cases stay rejected.
use super::*;
use crate::data_supply::registry::{CapabilitiesQuery, entries};
use crate::data_supply::wire::*;

fn normalized(name: &str, input: Value) -> Value {
    normalize(name, input).unwrap_or_else(|e| panic!("{}: {}: {input}", e.code, e.message))
}

fn decoded<T: serde::de::DeserializeOwned>(name: &str, input: Value) -> Result<T, String> {
    let value = normalize(name, input).map_err(|e| format!("{}: {}", e.code, e.message))?;
    serde_json::from_value(value).map_err(|e| e.to_string())
}

fn rejection(name: &str, input: Value) -> (String, String) {
    let error = normalize(name, input).unwrap_err();
    (error.code, error.message)
}

fn daily(extra: Value) -> Value {
    let mut base = json!({
        "dataset": "market.daily",
        "contract_version": "v1",
        "selection": {"kind": "single", "entity": {"exchange": "sh", "code": "600000"}},
        "filter": {"kind": "daily", "start_date": "2025-01-01", "end_date": "2025-12-31"},
        "version": {"kind": "latest_per_entity"},
    });
    base.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    base
}

/// §2.1.1 #1: `entity: "sh.600000"` used to fail with "expected struct
/// Security"; both spellings now decode to the same identity.
#[test]
fn rejection_01_entity_text_and_object_decode_to_the_same_request() {
    let text = daily(json!({"selection": {"kind": "single", "entity": "sh.600000"}}));
    let object = daily(json!({}));
    assert!(
        serde_json::from_value::<Query>(text.clone()).is_err(),
        "the raw contract must keep requiring the object form"
    );
    let friendly: Query = decoded("query_data", text).unwrap();
    let contract: Query = decoded("query_data", object).unwrap();
    assert_eq!(friendly, contract);
    assert_eq!(
        friendly.selection,
        Selection::Single {
            entity: Security {
                exchange: Exchange::Sh,
                code: "600000".into(),
            }
        }
    );
    // Batch and pinned-version maps take the same spelling.
    let batch: Query = decoded(
        "query_data",
        daily(json!({"selection": {"kind": "batch", "entities": ["sh.600000", "sz.000001"]}})),
    )
    .unwrap();
    let Selection::Batch { entities } = batch.selection else {
        panic!("expected batch selection");
    };
    assert_eq!(entities.len(), 2);
    assert_eq!(entities[1].symbol(), "sz.000001");
    let pinned: Query = decoded(
        "query_data",
        daily(
            json!({"version": {"kind": "explicit_version_map", "versions": [
                {"entity": "sh.600000", "version_id": "v1"}
            ]}}),
        ),
    )
    .unwrap();
    assert_eq!(
        pinned.version,
        VersionChoice::ExplicitVersionMap {
            versions: vec![VersionBinding {
                entity: Security {
                    exchange: Exchange::Sh,
                    code: "600000".into()
                },
                version_id: "v1".into(),
            }]
        }
    );
}

/// §2.1.1 #2: `version.kind=latest` was an unknown variant; it is now an
/// alias, and an omitted `version` means `latest_per_entity`.
#[test]
fn rejection_02_version_defaults_and_accepts_the_latest_alias() {
    let mut omitted = daily(json!({}));
    omitted.as_object_mut().unwrap().remove("version");
    let query: Query = decoded("query_data", omitted).unwrap();
    assert_eq!(query.version, VersionChoice::LatestPerEntity);
    let alias: Query =
        decoded("query_data", daily(json!({"version": {"kind": "latest"}}))).unwrap();
    assert_eq!(alias.version, VersionChoice::LatestPerEntity);
    // An explicit pin is unchanged and only the `latest` alias is widened.
    let pinned: Query = decoded(
        "query_data",
        daily(json!({"version": {"kind": "explicit_version_map", "versions": []}})),
    )
    .unwrap();
    assert_eq!(
        pinned.version,
        VersionChoice::ExplicitVersionMap { versions: vec![] }
    );
    assert!(decoded::<Query>("query_data", daily(json!({"version": {"kind": "newest"}}))).is_err());
}

/// §2.1.1 #3: `20250101` was "invalid characters"; both spellings normalize
/// to the same `YYYY-MM-DD` value.
#[test]
fn rejection_03_dates_accept_compact_and_dashed_forms() {
    let compact: Query = decoded(
        "query_data",
        daily(
            json!({"filter": {"kind": "daily", "start_date": "20250101", "end_date": "20251231"}}),
        ),
    )
    .unwrap();
    let dashed: Query = decoded("query_data", daily(json!({}))).unwrap();
    assert_eq!(compact.filter, dashed.filter);
    assert_eq!(
        compact.filter,
        Filter::Daily {
            start_date: NaiveDate::from_ymd_opt(2025, 1, 1).unwrap(),
            end_date: NaiveDate::from_ymd_opt(2025, 12, 31).unwrap(),
        }
    );
    // Range writes and plan/run inputs take the same spelling.
    let write: RangeWrite = decoded(
        "update_data",
        json!({"request_id": "r", "dataset": "market.daily", "items": [
            {"entity": "sh.600000", "base_version_id": "b", "start_date": "20200106", "end_date": "20200204"}
        ]}),
    )
    .unwrap();
    let RangeWrite::Daily { items, .. } = write else {
        panic!("expected daily range write");
    };
    assert_eq!(
        items.unwrap()[0].start_date,
        NaiveDate::from_ymd_opt(2020, 1, 6).unwrap()
    );
    let run: RunMarketUpdate = decoded(
        "run_market_update",
        json!({"dataset": "market.daily", "request_id": "r", "target_day": "20260130", "plan_id": "0".repeat(64)}),
    )
    .unwrap();
    assert_eq!(
        run.target_day(),
        NaiveDate::from_ymd_opt(2026, 1, 30).unwrap()
    );
    // An impossible calendar day names the field and both accepted formats.
    let (code, message) = rejection(
        "query_data",
        daily(
            json!({"filter": {"kind": "daily", "start_date": "20250230", "end_date": "2025-12-31"}}),
        ),
    );
    assert_eq!(code, "invalid_arguments");
    assert!(message.contains("$.filter.start_date"), "{message}");
    assert!(message.contains("YYYYMMDD"), "{message}");
}

/// §2.1.1 #4: a genuinely missing range field still fails, but the message
/// names the field and the accepted formats.
#[test]
fn rejection_04_missing_range_field_names_the_field_and_format() {
    let (code, message) = rejection(
        "query_data",
        daily(json!({"filter": {"kind": "daily", "end_date": "2025-12-31"}})),
    );
    assert_eq!(code, "invalid_arguments");
    assert!(message.contains("filter.start_date"), "{message}");
    assert!(message.contains("YYYYMMDD"), "{message}");
    let (code, message) = rejection(
        "query_data",
        json!({
            "dataset": "market.yearly",
            "selection": {"kind": "single", "entity": "sh.600000"},
            "filter": {"kind": "yearly", "start_year": 2020},
        }),
    );
    assert_eq!(code, "invalid_arguments");
    assert!(message.contains("filter.end_year"), "{message}");
}

/// §2.1.1 #5 / §7.2 row 3: every tool whose input contract has a
/// `contract_version` defaults it to v1.
#[test]
fn rejection_05_contract_version_defaults_to_v1() {
    let value = normalized("query_data", daily(json!({})));
    assert_eq!(value["contract_version"], "v1");
    for (name, input) in [
        ("get_task", json!({"task_id": "t"})),
        ("get_request", json!({"request_id": "r"})),
        ("cancel_task", json!({"task_id": "t"})),
        (
            "list_data_versions",
            json!({"dataset": "market.daily", "selection": {"kind": "single", "entity": "sh.600000"}}),
        ),
        ("plan_market_update", json!({"dataset": "market.daily"})),
        (
            "verify_data_gap",
            json!({"request_id": "r", "target": {"symbol": "sh.600000", "period": "day", "start_date": "20250101", "end_date": "20250102"}}),
        ),
        (
            "retry_task",
            json!({"original_task_id": "t", "new_request_id": "r", "item_ids": ["i"]}),
        ),
    ] {
        assert_eq!(
            normalized(name, input)["contract_version"],
            "v1",
            "{name} must default contract_version"
        );
    }
    // A non-v1 value is left untouched: `dispatch` keeps rejecting it with
    // `unsupported_contract` before any handler sees the request.
    let untouched = normalized("query_data", daily(json!({"contract_version": "v2"})));
    assert_eq!(untouched["contract_version"], "v2");
}

/// §2.1.1 #8 / §7.2 row 3: `get_data_capabilities` has no contract and keeps
/// rejecting `contract_version`, naming the fields it does take.
#[test]
fn rejection_08_capabilities_still_rejects_contract_version() {
    assert_eq!(normalized("get_data_capabilities", json!({})), json!({}));
    let error =
        decoded::<CapabilitiesQuery>("get_data_capabilities", json!({"contract_version": "v1"}))
            .unwrap_err();
    assert!(
        error.contains("unknown field `contract_version`"),
        "{error}"
    );
    assert!(error.contains("`dataset`"), "{error}");
    assert!(error.contains("`operation`"), "{error}");
}

/// §2.1.1 #7: an extra field is still rejected; normalization is not "accept
/// everything".
#[test]
fn rejection_07_unknown_fields_are_still_rejected() {
    let error = decoded::<Query>("query_data", daily(json!({"limit": 10}))).unwrap_err();
    assert!(error.contains("unknown field `limit`"), "{error}");
}

/// §2.1.1 #6 / §7.2 row 6: a cross-dataset selection is still rejected, and
/// the message lists the kinds the named dataset allows.
#[test]
fn rejection_06_cross_dataset_selection_names_the_allowed_kinds() {
    let (code, message) = rejection(
        "query_data",
        daily(json!({"selection": {"kind": "catalog", "scope": "current_sh_sz_a"}})),
    );
    assert_eq!(code, "invalid_arguments");
    assert!(message.contains("\"catalog\""), "{message}");
    assert!(message.contains("\"single\""), "{message}");
    assert!(message.contains("\"batch\""), "{message}");
    assert!(message.contains("market.daily"), "{message}");
    let (code, message) = rejection(
        "list_data_versions",
        json!({"dataset": "securities.catalog", "selection": {"kind": "single", "entity": "sh.600000"}}),
    );
    assert_eq!(code, "invalid_arguments");
    assert!(message.contains("\"catalog\""), "{message}");
    assert!(message.contains("securities.catalog"), "{message}");
}

/// §2.1.1 #10 / §7.2 row 7: every operation that production tasks actually
/// carry can be used as a filter (519 of 579 production tasks are
/// `update_data`).
#[test]
fn rejection_10_task_operations_cover_every_production_operation() {
    for operation in [
        "start_collection",
        "update_data",
        "backfill_data",
        "republish_data",
        "retry_task",
        "verify_stopped_writes",
        "verify_publication_result",
        "verify_data_gap",
        "inspect_verification_task",
    ] {
        let query: TasksQuery = decoded("list_tasks", json!({"operation": operation}))
            .unwrap_or_else(|e| panic!("{operation}: {e}"));
        assert_eq!(
            serde_json::to_value(query.operation.unwrap()).unwrap(),
            json!(operation)
        );
    }
    // `plan_market_update` creates no task and `run_market_update` tasks carry
    // `update_data`; inventing those enum values would silently match nothing.
    for absent in ["plan_market_update", "run_market_update"] {
        assert!(decoded::<TasksQuery>("list_tasks", json!({"operation": absent})).is_err());
    }
    assert!(decoded::<TasksQuery>("list_tasks", json!({"operation": "query"})).is_err());
}

/// §2.1.1 #12 / §7.2 row 11: `get_task` and `get_task_progress` agree that
/// the version is v1.
#[test]
fn rejection_12_task_reads_agree_on_contract_version() {
    let task: TaskV1 = decoded("get_task", json!({"task_id": "t"})).unwrap();
    assert_eq!(task.contract_version, ContractVersion::V1);
    let progress: ProgressQuery = decoded(
        "get_task_progress",
        json!({"task_id": "t", "contract_version": "v1"}),
    )
    .unwrap();
    assert_eq!(progress.task_id, "t");
    let tasks: TasksQuery = decoded("list_tasks", json!({"contract_version": "v1"})).unwrap();
    assert_eq!(tasks.page_size, default_page());
}

/// §2.1.1 #13: the display name in `code` is still rejected, and the message
/// says how to pass the identity instead.
#[test]
fn rejection_13_display_name_in_code_stays_rejected() {
    let query: Query = decoded(
        "query_data",
        daily(json!({"selection": {"kind": "single", "entity": {"exchange": "sh", "code": "sh.600000"}}})),
    )
    .unwrap();
    let Selection::Single { entity } = query.selection else {
        panic!("expected single selection");
    };
    let message = entity.validate().unwrap_err();
    assert!(message.contains("six digits"), "{message}");
    assert!(message.contains("sh.600000"), "{message}");
}

/// §2.1.1 #14 / §7.1 row 5: catalog reads may omit `filter`; market reads
/// must give a range.
#[test]
fn rejection_14_catalog_filter_defaults_and_market_filter_is_required() {
    let catalog: Query = decoded(
        "query_data",
        json!({"dataset": "securities.catalog", "selection": {"kind": "catalog", "scope": "current_sh_sz_a"}}),
    )
    .unwrap();
    assert_eq!(
        catalog.filter,
        Filter::Catalog {
            code: None,
            name_query: None,
            industry_code: None,
        }
    );
    assert_eq!(catalog.version, VersionChoice::LatestPerEntity);
    let (code, message) = rejection(
        "query_data",
        json!({
            "dataset": "market.daily",
            "selection": {"kind": "single", "entity": "sh.600000"},
        }),
    );
    assert_eq!(code, "invalid_arguments");
    assert!(message.contains("filter.kind=daily"), "{message}");
    assert!(message.contains("start_date"), "{message}");
}

/// §7.2 row 8: `verify_data_gap` names its subject `symbol`; the text and the
/// identity object normalize to the same canonical text.
#[test]
fn symbol_accepts_text_and_object_with_the_same_result() {
    let text: crate::gap_verification::wire::VerifyDataGap = decoded(
        "verify_data_gap",
        json!({"request_id": "r", "target": {"symbol": "sh.600006", "period": "day", "start_date": "20250101", "end_date": "20250110"}}),
    )
    .unwrap();
    let object: crate::gap_verification::wire::VerifyDataGap = decoded(
        "verify_data_gap",
        json!({"request_id": "r", "target": {"symbol": {"exchange": "sh", "code": "600006"}, "period": "day", "start_date": "20250101", "end_date": "20250110"}}),
    )
    .unwrap();
    assert_eq!(text.target.symbol, object.target.symbol);
    assert_eq!(text.target.start_date, object.target.start_date);
    assert_eq!(text.target.end_date, object.target.end_date);
    assert_eq!(text.target.symbol, "sh.600006");
    assert_eq!(
        text.target.start_date,
        NaiveDate::from_ymd_opt(2025, 1, 1).unwrap()
    );
    let (code, message) = rejection(
        "verify_data_gap",
        json!({"request_id": "r", "target": {"symbol": "600006", "period": "day", "start_date": "20250101", "end_date": "20250110"}}),
    );
    assert_eq!(code, "invalid_arguments");
    assert!(message.contains("$.target.symbol"), "{message}");
}

/// §7.2 row 10: `expected_revision` is optional; an explicit value keeps the
/// strict CAS precondition.
#[test]
fn cancel_expected_revision_is_optional() {
    let absent: CancelV1 = decoded("cancel_task", json!({"task_id": "t"})).unwrap();
    assert_eq!(absent.expected_revision, None);
    let explicit: CancelV1 = decoded(
        "cancel_task",
        json!({"task_id": "t", "expected_revision": 7}),
    )
    .unwrap();
    assert_eq!(explicit.expected_revision, Some(7));
}

/// AC-V8-11: `as_of`/`known_at` are reserved names, not bare unknown fields.
#[test]
fn as_of_and_known_at_are_reserved_for_m2() {
    for field in ["as_of", "known_at"] {
        for name in ["query_data", "get_data_availability", "list_data_versions"] {
            let mut input = daily(json!({}));
            input[field] = json!("2025-01-01");
            let (code, message) = rejection(name, input);
            assert_eq!(code, "reserved_for_m2", "{name}/{field}");
            assert!(message.contains(field), "{message}");
            assert!(message.contains("M2"), "{message}");
        }
    }
}

/// BR-V8-10: the friendly and the contract spelling must produce the same
/// normalized request — the same typed value, the same intent payload, and
/// therefore the same idempotency digest.
#[test]
fn friendly_and_contract_spellings_normalize_to_the_same_request() {
    let friendly = json!({
        "dataset": "market.daily",
        "selection": {"kind": "single", "entity": "sh.600000"},
        "filter": {"kind": "daily", "start_date": "20250101", "end_date": "20251231"},
        "version": {"kind": "latest"},
    });
    assert_eq!(
        normalized("query_data", friendly),
        normalized("query_data", daily(json!({})))
    );
    let friendly_write = json!({
        "request_id": "r",
        "dataset": "market.daily",
        "selection": {"kind": "explicit", "securities": ["sh.600000"]},
    });
    let contract_write = json!({
        "request_id": "r",
        "dataset": "market.daily",
        "contract_version": "v1",
        "selection": {"kind": "explicit", "securities": [{"exchange": "sh", "code": "600000"}]},
    });
    assert_eq!(
        normalized("start_collection", friendly_write),
        normalized("start_collection", contract_write)
    );
    let friendly_range = json!({
        "request_id": "r",
        "dataset": "market.daily",
        "items": [{"entity": "sh.600000", "base_version_id": "b", "start_date": "20200106", "end_date": "20200204"}],
    });
    let contract_range = json!({
        "request_id": "r",
        "dataset": "market.daily",
        "contract_version": "v1",
        "items": [{"entity": {"exchange": "sh", "code": "600000"}, "base_version_id": "b", "start_date": "2020-01-06", "end_date": "2020-02-04"}],
    });
    assert_eq!(
        normalized("update_data", friendly_range.clone()),
        normalized("update_data", contract_range)
    );
    // The intent payload is what the acceptance digest covers.
    let a: RangeWrite = decoded("update_data", friendly_range).unwrap();
    let b: RangeWrite = decoded("update_data", contract_range_for_payload()).unwrap();
    assert_eq!(a.payload().unwrap(), b.payload().unwrap());
    let start: Collection = decoded(
        "start_collection",
        json!({"request_id": "r", "dataset": "market.daily", "selection": {"kind": "explicit", "securities": ["sh.600000"]}}),
    )
    .unwrap();
    let start_contract: Collection = decoded(
        "start_collection",
        json!({"request_id": "r", "dataset": "market.daily", "contract_version": "v1", "selection": {"kind": "explicit", "securities": [{"exchange": "sh", "code": "600000"}]}}),
    )
    .unwrap();
    assert_eq!(start.payload(), start_contract.payload());
}

fn contract_range_for_payload() -> Value {
    json!({
        "request_id": "r",
        "dataset": "market.daily",
        "contract_version": "v1",
        "items": [{"entity": {"exchange": "sh", "code": "600000"}, "base_version_id": "b", "start_date": "2020-01-06", "end_date": "2020-02-04"}],
    })
}

/// Normalization runs before every decode; running it twice must not move.
#[test]
fn normalize_is_idempotent() {
    for (name, input) in [
        ("query_data", daily(json!({}))),
        (
            "query_data",
            daily(
                json!({"selection": {"kind": "batch", "entities": ["sh.600000"]}, "filter": {"kind": "daily", "start_date": "20250101", "end_date": "20251231"}}),
            ),
        ),
        (
            "start_collection",
            json!({"request_id": "r", "dataset": "market.daily", "selection": {"kind": "explicit", "securities": ["sh.600000"]}}),
        ),
        (
            "verify_data_gap",
            json!({"request_id": "r", "target": {"symbol": {"exchange": "sh", "code": "600006"}, "period": "day", "start_date": "20250101", "end_date": "20250110"}}),
        ),
        ("get_data_capabilities", json!({})),
    ] {
        let once = normalized(name, input.clone());
        assert_eq!(normalized(name, once.clone()), once, "{name}");
    }
}

/// The defaulting list is derived from the registry, so a new tool cannot
/// silently skip the `contract_version` default.
#[test]
fn contract_tools_match_the_registry() {
    fn mentions(value: &Value, key: &str) -> bool {
        match value {
            Value::Object(map) => {
                map.contains_key(key) || map.values().any(|child| mentions(child, key))
            }
            Value::Array(items) => items.iter().any(|child| mentions(child, key)),
            _ => false,
        }
    }
    for entry in entries() {
        assert_eq!(
            mentions(&entry.input, "contract_version"),
            CONTRACT_TOOLS.contains(&entry.name),
            "{}",
            entry.name
        );
    }
}
