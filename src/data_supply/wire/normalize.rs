//! Decode-layer input normalization (v8 PR-1, #387 §7).
//!
//! This layer only widens **which spellings decode**. It never changes what a
//! request means: the normalized value feeds the same typed decode, the same
//! contract `normalize`, and therefore the same intent payload and the same
//! idempotency digest (BR-V8-10). Identity, version pinning, time semantics
//! and acceptance rules stay where they are.
//!
//! Rules (design §7.1):
//!
//! | input | friendly spelling | contract form |
//! |---|---|---|
//! | entity | `"sh.600000"` | `{"exchange":"sh","code":"600000"}` |
//! | symbol | `{"exchange","code"}` | `"sh.600000"` |
//! | date | `YYYYMMDD` | `YYYY-MM-DD` |
//! | `contract_version` | omitted | `"v1"` |
//! | `version` | omitted / `{"kind":"latest"}` | `{"kind":"latest_per_entity"}` |
//! | `filter` (catalog) | omitted | `{"kind":"catalog"}` |
//!
//! Everything else stays a rejection, with a message that names the field and
//! the accepted values (`reserved_for_m2` for `as_of`/`known_at`).
use super::Security;
use crate::mcp::ApiError;
use chrono::NaiveDate;
use serde_json::{Map, Value, json};

/// Tools whose input contract carries `contract_version`; a missing value
/// defaults to `"v1"`. Kept in sync with the registry by
/// `contract_tools_match_the_registry`.
const CONTRACT_TOOLS: &[&str] = &[
    "query_data",
    "list_data_versions",
    "get_data_availability",
    "get_yearly_base_years",
    "update_data",
    "backfill_data",
    "republish_data",
    "start_collection",
    "retry_task",
    "get_task",
    "get_request",
    "cancel_task",
    "verify_stopped_writes",
    "verify_publication_result",
    "inspect_verification_task",
    "verify_data_gap",
    "plan_market_update",
    "run_market_update",
    "compute_metrics",
    // v8 PR-5 (#387 §14): the task surface gained `contract_version` so v2
    // can be asked for explicitly; omitting it still means v1 (design §7.2
    // row 11).
    "list_tasks",
    "get_task_progress",
];
/// Read tools whose `selection.kind` is dataset-discriminated (design §7.2
/// row 6): the rejection lists the kinds the named dataset allows.
const SELECTION_TOOLS: &[&str] = &["query_data", "list_data_versions", "get_data_availability"];
/// Tools that take `version`/`filter` (design §7.1 rows 4 and 5).
const RANGE_TOOLS: &[&str] = &["query_data", "get_data_availability"];
/// Names reserved for M2 point-in-time semantics (design §4.6, AC-V8-11).
const RESERVED: &[&str] = &["as_of", "known_at"];
/// One decoded JSON object. Aliased so signatures carry no generic comma.
type Fields = Map<String, Value>;

/// Normalize one tool call's raw arguments before typed decoding.
///
/// Idempotent: normalizing an already-normalized request returns it unchanged.
pub fn normalize(name: &str, mut input: Value) -> Result<Value, ApiError> {
    if !input.is_object() {
        return Ok(input);
    }
    reserved(name, &input)?;
    if let Value::Object(map) = &mut input {
        contract_version(name, map);
    }
    shape(name, &mut input)?;
    walk(&mut input, "$")?;
    Ok(input)
}

fn invalid(message: String) -> ApiError {
    ApiError::new("invalid_arguments", message)
}

fn reserved(name: &str, input: &Value) -> Result<(), ApiError> {
    let Some(map) = input.as_object() else {
        return Ok(());
    };
    let Some(field) = RESERVED.iter().find(|field| map.contains_key(**field)) else {
        return Ok(());
    };
    Err(ApiError::new(
        "reserved_for_m2",
        format!(
            "{field} is reserved for M2 point-in-time semantics and is not implemented in v8; \
             remove it from {name} (see docs/tasks/issue-391-m2-security-master.md)"
        ),
    ))
}

fn contract_version(name: &str, map: &mut Fields) {
    if map.contains_key("contract_version") || !CONTRACT_TOOLS.contains(&name) {
        return;
    }
    map.insert("contract_version".into(), json!("v1"));
}

/// Cross-field defaults and dataset-dependent rejections. Only the fields the
/// design names; a missing `dataset` stays a plain decode error.
fn shape(name: &str, input: &mut Value) -> Result<(), ApiError> {
    let Some(map) = input.as_object_mut() else {
        return Ok(());
    };
    let Some(dataset) = map
        .get("dataset")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return Ok(());
    };
    if SELECTION_TOOLS.contains(&name) {
        selection_kind(name, map, &dataset)?;
    }
    if RANGE_TOOLS.contains(&name) {
        version(map);
        filter(map, &dataset)?;
    }
    Ok(())
}

fn selection_kind(name: &str, map: &Fields, dataset: &str) -> Result<(), ApiError> {
    let allowed: &[&str] = match dataset {
        "securities.catalog" => &["catalog"],
        "market.daily" | "market.index" | "market.adjustment_events" => {
            &["single", "batch", "universe"]
        }
        "market.yearly" => &["single", "batch"],
        "market.calendar" => &["market"],
        _ => return Ok(()),
    };
    let Some(kind) = map
        .get("selection")
        .and_then(|selection| selection.get("kind"))
        .and_then(Value::as_str)
    else {
        return Ok(());
    };
    if allowed.contains(&kind) {
        return Ok(());
    }
    Err(invalid(format!(
        "selection.kind {kind:?} is not valid for dataset {dataset}: {name} accepts {} for {dataset}",
        quoted(allowed)
    )))
}

fn version(map: &mut Fields) {
    if !map.contains_key("version") {
        map.insert("version".into(), json!({"kind": "latest_per_entity"}));
        return;
    }
    if let Some(Value::Object(choice)) = map.get_mut("version")
        && choice.get("kind").and_then(Value::as_str) == Some("latest")
    {
        choice.insert("kind".into(), json!("latest_per_entity"));
    }
}

fn filter(map: &mut Fields, dataset: &str) -> Result<(), ApiError> {
    let allowed: &[&str] = match dataset {
        "securities.catalog" => &["catalog"],
        "market.daily" | "market.index" => &["daily", "cross_section"],
        "market.yearly" => &["yearly"],
        "market.adjustment_events" | "market.calendar" => &["daily"],
        _ => return Ok(()),
    };
    let Some(filter) = map.get("filter").cloned() else {
        if dataset == "securities.catalog" {
            map.insert("filter".into(), json!({"kind": "catalog"}));
            return Ok(());
        }
        let (fields, format) = if dataset == "market.yearly" {
            ("start_year and end_year", "")
        } else {
            ("start_date and end_date", " (YYYY-MM-DD or YYYYMMDD)")
        };
        return Err(invalid(format!(
            "dataset {dataset} requires a date range: filter.kind={} with {fields}{format}",
            allowed[0]
        )));
    };
    let Some(kind) = filter.get("kind").and_then(Value::as_str) else {
        return Ok(());
    };
    if !allowed.contains(&kind) {
        return Err(invalid(format!(
            "filter.kind {kind:?} is not valid for dataset {dataset}: allowed {}",
            quoted(allowed)
        )));
    }
    let required: &[&str] = match kind {
        "daily" | "cross_section" => &["start_date", "end_date"],
        "yearly" => &["start_year", "end_year"],
        _ => return Ok(()),
    };
    for field in required {
        if filter.get(*field).is_none_or(Value::is_null) {
            return Err(invalid(format!(
                "filter.{field} is required for filter.kind={kind}{}",
                if kind == "daily" {
                    " (YYYY-MM-DD or YYYYMMDD)"
                } else {
                    ""
                }
            )));
        }
    }
    Ok(())
}

/// Recursive field-name walk: the same spellings apply at any depth, so
/// `selection.entity`, `items[].entity`, `version_ref.entity` and
/// `target.symbol` need no per-tool list. Unknown shapes are left to the
/// typed decode, which keeps its own messages.
fn walk(value: &mut Value, path: &str) -> Result<(), ApiError> {
    match value {
        Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                let path = format!("{path}.{key}");
                match key.as_str() {
                    "entity" => entity_at(child, &path)?,
                    "entities" | "securities" => entities_at(child, &path)?,
                    "symbol" => symbol_at(child, &path)?,
                    "start_date" | "end_date" | "target_day" => date_at(child, &path)?,
                    _ => walk(child, &path)?,
                }
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter_mut().enumerate() {
                walk(child, &format!("{path}[{index}]"))?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn entity_at(value: &mut Value, path: &str) -> Result<(), ApiError> {
    let Some(text) = value.as_str().map(str::to_owned) else {
        return Ok(());
    };
    let security =
        Security::parse(&text).map_err(|message| invalid(format!("{path}: {message}")))?;
    *value = serde_json::to_value(security).unwrap();
    Ok(())
}

fn entities_at(value: &mut Value, path: &str) -> Result<(), ApiError> {
    let Value::Array(items) = value else {
        return Ok(());
    };
    for (index, item) in items.iter_mut().enumerate() {
        entity_at(item, &format!("{path}[{index}]"))?;
    }
    Ok(())
}

/// `verify_data_gap` names its subject `symbol`; both the canonical text and
/// the shared identity object normalize to the canonical text (design §7.2
/// row 8). The six-digit rule stays the same as every other identity.
fn symbol_at(value: &mut Value, path: &str) -> Result<(), ApiError> {
    let security = match value.clone() {
        Value::String(text) => {
            Security::parse(&text).map_err(|message| invalid(format!("{path}: {message}")))?
        }
        other => serde_json::from_value::<Security>(other)
            .map_err(|error| invalid(format!("{path}: {error}")))?,
    };
    security
        .validate()
        .map_err(|message| invalid(format!("{path}: {message}")))?;
    *value = Value::String(security.symbol());
    Ok(())
}

fn date_at(value: &mut Value, path: &str) -> Result<(), ApiError> {
    let Some(text) = value.as_str().map(str::to_owned) else {
        return Ok(());
    };
    let day = parse_date(&text).ok_or_else(|| {
        invalid(format!(
            "{path} must be YYYY-MM-DD or YYYYMMDD, got {text:?}"
        ))
    })?;
    *value = Value::String(day.format("%Y-%m-%d").to_string());
    Ok(())
}

/// Accept both date spellings the project writes (design §7.2 row 4).
fn parse_date(text: &str) -> Option<NaiveDate> {
    let format = if text.len() == 8 && text.bytes().all(|b| b.is_ascii_digit()) {
        "%Y%m%d"
    } else {
        "%Y-%m-%d"
    };
    NaiveDate::parse_from_str(text, format).ok()
}

fn quoted(values: &[&str]) -> String {
    values
        .iter()
        .map(|value| format!("{value:?}"))
        .collect::<Vec<_>>()
        .join(" or ")
}

#[cfg(test)]
mod tests;
