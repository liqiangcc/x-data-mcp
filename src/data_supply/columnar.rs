//! Columnar read-model projection (v8 PR-3, #387 §5).
//!
//! Shape only: every cell carries the same value the v1 row carried
//! (`version_id`, exact decimal strings, `null`), and nothing here reads
//! storage. The three layouts (`panel`, `cross_section`, `table`) are pure
//! functions over the read snapshot, so the same projection serves the MCP
//! read surface now and the M1 research read model later (design §10.1,
//! separation points 4/5).
use super::wire::*;
use crate::market_storage::Error;
use chrono::{DateTime, NaiveDate, Utc};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// Envelope facts shared by every v2 layout.
pub struct PageMeta<'a> {
    pub snapshot_id: &'a str,
    pub snapshot_time: DateTime<Utc>,
    pub next_cursor: Option<&'a str>,
    pub page_complete: bool,
    pub limitations: &'a [String],
    /// M2 point-in-time facts (issue-391 §12.2).
    pub temporal: Option<&'a TemporalBlock>,
}

/// Allowed `fields` per dataset (v8 §4.2). Defaults are the design's core
/// column set; the remaining fields are opt-in so the common response stays
/// small (for example the catalog's industry status/reason).
pub const DAILY_FIELDS: &[&str] = &["open", "high", "low", "close", "volume", "amount"];
pub const YEARLY_FIELDS: &[&str] = &[
    "open",
    "high",
    "low",
    "close",
    "volume",
    "amount",
    "first_trading_date",
    "last_trading_date",
    "year_unfinished",
];
pub const CATALOG_FIELDS: &[&str] = &[
    "name",
    "industry_code",
    "industry_name",
    "industry_status",
    "industry_reason",
];
pub const CATALOG_DEFAULT: &[&str] = &["name", "industry_code", "industry_name"];
/// `market.adjustment_events`: the row key `effective_day` is implicit and
/// `known_at` is never emitted (M1 stores NULL, v8 §4.6).
pub const EVENT_FIELDS: &[&str] = &["category", "c1", "c2", "c3", "c4"];
/// `market.calendar`: the row key is the only column (v8 §4.2).
pub const CALENDAR_FIELDS: &[&str] = &["trade_day"];
/// M2 parsed-fact column sets (issue-391 §12.4). The row key is implicit:
/// `effective_from`/`effective_day`/`start_day`/`rule_id` depends on the
/// dataset.
pub const SECURITY_MASTER_FIELDS: &[&str] = &[
    "effective_from",
    "effective_to",
    "instrument_class",
    "issuer_key",
    "issuer_resolution",
    "listing_date",
    "listing_source",
    "delisting_date",
    "delisting_source",
    "arrangement_start",
    "arrangement_end",
    "known_at",
    "flags",
];
pub const SECURITY_MASTER_DEFAULT: &[&str] = &[
    "instrument_class",
    "issuer_resolution",
    "listing_date",
    "listing_source",
    "delisting_date",
    "delisting_source",
    "known_at",
    "flags",
];
pub const SECURITY_NAMES_FIELDS: &[&str] = &[
    "effective_from",
    "effective_to",
    "name",
    "certainty",
    "known_at",
    "source_kind",
];
pub const CAPITAL_EVENTS_FIELDS: &[&str] = &[
    "ordinal",
    "effective_day",
    "category",
    "float_after",
    "total_after",
    "known_at",
    "suspicious",
];
pub const RISK_WARNINGS_FIELDS: &[&str] = &[
    "effective_from",
    "effective_to",
    "normalized_status",
    "raw_status_code",
    "announced_at",
    "known_at",
    "source_kind",
];
pub const SUSPENSIONS_FIELDS: &[&str] = &[
    "start_day",
    "end_day",
    "certainty",
    "missing_days",
    "reason",
    "known_at",
];
pub const PRICE_LIMIT_RULES_FIELDS: &[&str] = &[
    "rule_id",
    "market",
    "board",
    "status_class",
    "listing_phase",
    "band_bp",
    "band_side",
    "rounding",
    "effective_from",
    "effective_to",
    "basis_ref",
    "basis_status",
];
/// `get_data_availability` columns (v8 §4.1): per-entity facts plus the
/// related-task ids; the task objects themselves are shared once under
/// `tasks`.
pub const AVAILABILITY_FIELDS: &[&str] = &[
    "availability",
    "integrity",
    "version_id",
    "coverage_evidence",
    "first_record",
    "last_record",
    "record_count",
    "freshness",
    "stale",
    "as_of_day",
    "reason",
    "confirmed_gaps",
    "unresolved_ranges",
    "limitations",
    "related_tasks",
    "related_tasks_total",
    "related_tasks_truncated",
    "universe",
];

pub fn query_fields(dataset: Dataset) -> (&'static [&'static str], &'static [&'static str]) {
    match dataset {
        Dataset::Catalog => (CATALOG_FIELDS, CATALOG_DEFAULT),
        Dataset::Daily => (DAILY_FIELDS, DAILY_FIELDS),
        Dataset::Yearly => (YEARLY_FIELDS, YEARLY_FIELDS),
        Dataset::Index => (DAILY_FIELDS, DAILY_FIELDS),
        Dataset::AdjustmentEvents => (EVENT_FIELDS, EVENT_FIELDS),
        Dataset::Calendar => (CALENDAR_FIELDS, CALENDAR_FIELDS),
        Dataset::SecurityMaster => (SECURITY_MASTER_FIELDS, SECURITY_MASTER_DEFAULT),
        Dataset::SecurityNames => (SECURITY_NAMES_FIELDS, SECURITY_NAMES_FIELDS),
        Dataset::CapitalEvents => (CAPITAL_EVENTS_FIELDS, CAPITAL_EVENTS_FIELDS),
        Dataset::RiskWarnings => (RISK_WARNINGS_FIELDS, RISK_WARNINGS_FIELDS),
        Dataset::Suspensions => (SUSPENSIONS_FIELDS, SUSPENSIONS_FIELDS),
        Dataset::PriceLimitRules => (PRICE_LIMIT_RULES_FIELDS, PRICE_LIMIT_RULES_FIELDS),
    }
}

fn invalid(message: String) -> Error {
    Error::Invalid(message)
}

/// Resolve `fields` against the dataset's allowed set. `None` selects the
/// core set; unknown or duplicate names are rejected with the allowed list
/// (design §4.3: the error names the options, not a bare "unknown field").
pub fn select(
    fields: Option<&[String]>,
    allowed: &[&str],
    default: &[&str],
    scope: &str,
) -> Result<Vec<String>, Error> {
    let Some(fields) = fields else {
        return Ok(default.iter().map(|field| (*field).to_owned()).collect());
    };
    if fields.is_empty() {
        return Err(invalid("fields must name at least one field".into()));
    }
    let mut seen = BTreeSet::new();
    for field in fields {
        if !allowed.contains(&field.as_str()) {
            return Err(invalid(format!(
                "unknown field {field:?} for {scope}; allowed fields: {}",
                allowed.join(", ")
            )));
        }
        if !seen.insert(field.as_str()) {
            return Err(invalid(format!("duplicate field {field:?}")));
        }
    }
    Ok(allowed
        .iter()
        .filter(|field| seen.contains(**field))
        .map(|field| (*field).to_owned())
        .collect())
}

/// Shared metadata fields: identical values are written once, per-entity
/// differences move to the `versions` row they belong to (design §5.1 rule 3,
/// with the correction that these are not constants in production — a
/// fail-closed coverage status differs per entity, see `docs/STATUS.md`).
const METADATA_KEYS: &[&str] = &[
    "source",
    "coverage_status",
    "coverage_reason",
    "units",
    "adjustment",
    "lineage",
    "index",
];

fn metadata_value(metadata: &Metadata, key: &str) -> Value {
    match key {
        "source" => json!(metadata.source),
        "coverage_status" => json!(metadata.coverage_status),
        "coverage_reason" => json!(metadata.coverage_reason),
        "units" => {
            if metadata.units.is_empty() {
                Value::Null
            } else {
                serde_json::to_value(&metadata.units).unwrap()
            }
        }
        "adjustment" => metadata
            .adjustment
            .as_ref()
            .map(|value| serde_json::to_value(value).unwrap())
            .unwrap_or(Value::Null),
        "lineage" => metadata
            .lineage
            .as_ref()
            .map(|value| serde_json::to_value(value).unwrap())
            .unwrap_or(Value::Null),
        "index" => metadata
            .index
            .as_ref()
            .map(|value| serde_json::to_value(value).unwrap())
            .unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

/// `(shared, per-manifest-entry overrides)`. Entries without metadata (absent
/// versions) take no part in the shared decision.
fn split_metadata(manifest: &[ManifestEntry]) -> (Map<String, Value>, Vec<Map<String, Value>>) {
    let mut shared = Map::new();
    let mut overrides = vec![Map::new(); manifest.len()];
    let present: Vec<usize> = manifest
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.metadata.is_some())
        .map(|(index, _)| index)
        .collect();
    if present.is_empty() {
        return (shared, overrides);
    }
    for key in METADATA_KEYS {
        let values: Vec<Value> = present
            .iter()
            .map(|index| metadata_value(manifest[*index].metadata.as_ref().unwrap(), key))
            .collect();
        if values.iter().all(Value::is_null) {
            continue;
        }
        if values.windows(2).all(|pair| pair[0] == pair[1]) {
            shared.insert((*key).to_owned(), values[0].clone());
        } else {
            for (position, index) in present.iter().enumerate() {
                if !values[position].is_null() {
                    overrides[*index].insert((*key).to_owned(), values[position].clone());
                }
            }
        }
    }
    (shared, overrides)
}

/// One `versions` row per manifest entry: the version list appears once and
/// `integrity_sha256` is gone (`version_id` is the digest, design §5.1 rule 2).
fn version_rows(manifest: &[ManifestEntry], overrides: &[Map<String, Value>]) -> Vec<Value> {
    manifest
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let mut row = Map::new();
            if let Some(entity) = &entry.entity {
                row.insert("entity".to_owned(), json!(entity.symbol()));
            }
            row.insert(
                "status".to_owned(),
                json!(match entry.status {
                    ManifestStatus::Present => "present",
                    ManifestStatus::Absent => "absent",
                }),
            );
            if let Some(reference) = &entry.version_ref {
                row.insert("version_id".to_owned(), json!(reference.version_id));
            }
            if let Some(metadata) = &entry.metadata {
                row.insert(
                    "published_at".to_owned(),
                    serde_json::to_value(metadata.published_at).unwrap(),
                );
            }
            if let Some(stale) = &entry.stale {
                row.insert("stale".to_owned(), json!(stale));
            }
            if let Some(day) = entry.as_of_day {
                row.insert("as_of_day".to_owned(), json!(day.to_string()));
            }
            if let Some(reason) = entry.reason {
                row.insert("reason".to_owned(), serde_json::to_value(reason).unwrap());
            }
            for (key, value) in &overrides[index] {
                row.insert(key.clone(), value.clone());
            }
            Value::Object(row)
        })
        .collect()
}

fn envelope(layout: &str, dataset: Dataset, meta: &PageMeta) -> Map<String, Value> {
    let mut out = Map::new();
    out.insert("dataset".to_owned(), json!(dataset));
    out.insert("contract_version".to_owned(), json!("v2"));
    out.insert("layout".to_owned(), json!(layout));
    out.insert("snapshot_id".to_owned(), json!(meta.snapshot_id));
    out.insert(
        "snapshot_time".to_owned(),
        serde_json::to_value(meta.snapshot_time).unwrap(),
    );
    out.insert(
        "next_cursor".to_owned(),
        meta.next_cursor.map_or(Value::Null, |cursor| json!(cursor)),
    );
    out.insert("page_complete".to_owned(), json!(meta.page_complete));
    if !meta.limitations.is_empty() {
        out.insert(
            "limitations".to_owned(),
            serde_json::to_value(meta.limitations).unwrap(),
        );
    }
    if let Some(temporal) = meta.temporal {
        out.insert(
            "temporal".to_owned(),
            serde_json::to_value(temporal).unwrap(),
        );
    }
    out
}

fn is_absent(value: &Value) -> bool {
    value.is_null() || value.as_array().is_some_and(|items| items.is_empty())
}

/// One-dimensional column cells keyed by field name.
type Cells<'a> = BTreeMap<&'a str, Vec<Value>>;

/// Position-aligned column map: a column whose every cell is null/empty is
/// omitted; a partially null column keeps `null` cells so rows never shift
/// (design §5.2).
fn columns(fields: &[String], cells: &Cells<'_>) -> Map<String, Value> {
    let mut out = Map::new();
    for field in fields {
        let Some(values) = cells.get(field.as_str()) else {
            continue;
        };
        if values.iter().all(is_absent) {
            continue;
        }
        out.insert(field.clone(), Value::Array(values.clone()));
    }
    out
}

fn market_cell(row: &Row, field: &str) -> Value {
    let Row::Market {
        open,
        high,
        low,
        close,
        volume,
        amount,
        first_trading_date,
        last_trading_date,
        year_unfinished,
        ..
    } = row
    else {
        return Value::Null;
    };
    match field {
        "open" => json!(open),
        "high" => json!(high),
        "low" => json!(low),
        "close" => json!(close),
        "volume" => volume.as_ref().map_or(Value::Null, |value| json!(value)),
        "amount" => amount.as_ref().map_or(Value::Null, |value| json!(value)),
        "first_trading_date" => {
            first_trading_date.map_or(Value::Null, |day| json!(day.to_string()))
        }
        "last_trading_date" => last_trading_date.map_or(Value::Null, |day| json!(day.to_string())),
        "year_unfinished" => year_unfinished.map_or(Value::Null, |value| json!(value)),
        _ => Value::Null,
    }
}

fn event_cell(row: &Row, field: &str) -> Value {
    let Row::AdjustmentEvent {
        category,
        c1,
        c2,
        c3,
        c4,
        ..
    } = row
    else {
        return Value::Null;
    };
    match field {
        "category" => json!(category),
        "c1" => json!(c1),
        "c2" => json!(c2),
        "c3" => json!(c3),
        "c4" => json!(c4),
        _ => Value::Null,
    }
}

fn calendar_cell(row: &Row, field: &str) -> Value {
    let Row::Calendar { trade_day } = row else {
        return Value::Null;
    };
    match field {
        "trade_day" => json!(trade_day.to_string()),
        _ => Value::Null,
    }
}

fn catalog_cell(row: &Row, field: &str) -> Value {
    let Row::Catalog { name, industry, .. } = row else {
        return Value::Null;
    };
    match field {
        "name" => json!(name),
        "industry_code" => match industry {
            Industry::Assigned { code, .. } => json!(code),
            _ => Value::Null,
        },
        "industry_name" => match industry {
            Industry::Assigned { name, .. } => json!(name),
            _ => Value::Null,
        },
        "industry_status" => match industry {
            Industry::Assigned { .. } => Value::Null,
            Industry::Missing { .. } => json!("missing"),
            Industry::Conflicted { .. } => json!("conflicted"),
        },
        "industry_reason" => match industry {
            Industry::Assigned { .. } => Value::Null,
            Industry::Missing { reason } | Industry::Conflicted { reason } => json!(reason),
        },
        _ => Value::Null,
    }
}

/// `layout:"panel"`: one row key per entity (`dates[entity_index]`), values
/// under `columns[field][entity_index][row_index]`.
pub fn panel(
    dataset: Dataset,
    manifest: &[ManifestEntry],
    entities: &[Security],
    rows: &[Row],
    fields: &[String],
    meta: &PageMeta,
) -> Value {
    let (shared, overrides) = split_metadata(manifest);
    let position: BTreeMap<String, usize> = entities
        .iter()
        .enumerate()
        .map(|(index, entity)| (entity.symbol(), index))
        .collect();
    let mut dates: Vec<Vec<Value>> = vec![Vec::new(); entities.len()];
    let mut cells: BTreeMap<&str, Vec<Vec<Value>>> = fields
        .iter()
        .map(|field| (field.as_str(), vec![Vec::new(); entities.len()]))
        .collect();
    for row in rows {
        let Row::Market { entity, date, .. } = row else {
            continue;
        };
        let Some(index) = position.get(&entity.symbol()).copied() else {
            continue;
        };
        dates[index].push(json!(date.to_string()));
        for field in fields {
            cells
                .get_mut(field.as_str())
                .unwrap()
                .get_mut(index)
                .unwrap()
                .push(market_cell(row, field));
        }
    }
    let mut out = envelope("panel", dataset, meta);
    out.insert(
        "entities".to_owned(),
        Value::Array(
            entities
                .iter()
                .map(|entity| json!(entity.symbol()))
                .collect(),
        ),
    );
    out.insert(
        "dates".to_owned(),
        Value::Array(dates.into_iter().map(Value::Array).collect()),
    );
    out.insert(
        "versions".to_owned(),
        Value::Array(version_rows(manifest, &overrides)),
    );
    if !shared.is_empty() {
        out.insert("metadata".to_owned(), Value::Object(shared));
    }
    // `columns` is two-dimensional; build it from the per-entity vectors.
    let mut columns = Map::new();
    for field in fields {
        let per_entity = cells.get(field.as_str()).unwrap();
        if per_entity.iter().flatten().all(is_absent) {
            continue;
        }
        columns.insert(
            field.clone(),
            Value::Array(
                per_entity
                    .iter()
                    .map(|values| Value::Array(values.clone()))
                    .collect(),
            ),
        );
    }
    out.insert("columns".to_owned(), Value::Object(columns));
    out.insert("row_count".to_owned(), json!(rows.len()));
    out.insert("selection_complete".to_owned(), json!(true));
    Value::Object(out)
}

/// `layout:"table"` for `securities.catalog`: no `dates`; `columns[field]` is
/// one-dimensional and `entities` names the page's rows.
pub fn catalog_table(
    manifest: &[ManifestEntry],
    rows: &[Row],
    fields: &[String],
    meta: &PageMeta,
) -> Value {
    let (shared, overrides) = split_metadata(manifest);
    let mut entities = Vec::new();
    let mut cells: Cells<'_> = fields
        .iter()
        .map(|field| (field.as_str(), Vec::new()))
        .collect();
    for row in rows {
        let Row::Catalog { entity, .. } = row else {
            continue;
        };
        entities.push(json!(entity.symbol()));
        for field in fields {
            cells
                .get_mut(field.as_str())
                .unwrap()
                .push(catalog_cell(row, field));
        }
    }
    let mut out = envelope("table", Dataset::Catalog, meta);
    out.insert("entities".to_owned(), Value::Array(entities));
    out.insert(
        "versions".to_owned(),
        Value::Array(version_rows(manifest, &overrides)),
    );
    if !shared.is_empty() {
        out.insert("metadata".to_owned(), Value::Object(shared));
    }
    out.insert("columns".to_owned(), Value::Object(columns(fields, &cells)));
    out.insert("row_count".to_owned(), json!(rows.len()));
    Value::Object(out)
}

/// `layout:"table"` for `market.adjustment_events` and `market.calendar`.
/// Events keep one `entities` entry per row (a batch spans several symbols);
/// the calendar is market-level and has none.
pub fn row_table(
    dataset: Dataset,
    manifest: &[ManifestEntry],
    rows: &[Row],
    fields: &[String],
    meta: &PageMeta,
) -> Value {
    let (shared, overrides) = split_metadata(manifest);
    let entity_bound = dataset == Dataset::AdjustmentEvents;
    let mut entities = Vec::new();
    let mut cells: Cells<'_> = fields
        .iter()
        .map(|field| (field.as_str(), Vec::new()))
        .collect();
    for row in rows {
        if entity_bound {
            let Row::AdjustmentEvent { entity, .. } = row else {
                continue;
            };
            entities.push(json!(entity.symbol()));
        }
        for field in fields {
            let cell = match dataset {
                Dataset::AdjustmentEvents => event_cell(row, field),
                Dataset::Calendar => calendar_cell(row, field),
                _ => Value::Null,
            };
            cells.get_mut(field.as_str()).unwrap().push(cell);
        }
    }
    let mut out = envelope("table", dataset, meta);
    if entity_bound {
        out.insert("entities".to_owned(), Value::Array(entities));
    }
    out.insert(
        "versions".to_owned(),
        Value::Array(version_rows(manifest, &overrides)),
    );
    if !shared.is_empty() {
        out.insert("metadata".to_owned(), Value::Object(shared));
    }
    out.insert("columns".to_owned(), Value::Object(columns(fields, &cells)));
    out.insert("row_count".to_owned(), json!(rows.len()));
    Value::Object(out)
}

/// `layout:"table"` for the M2 parsed-fact datasets (issue-391 §12.3).
/// Columns come from [`Row::Fact`] cells; entity-bound datasets list one
/// `entities` entry per row, the whole-market rule set lists none.
pub fn fact_table(
    dataset: Dataset,
    manifest: &[ManifestEntry],
    rows: &[Row],
    fields: &[String],
    meta: &PageMeta,
) -> Value {
    let (shared, overrides) = split_metadata(manifest);
    let entity_bound = dataset.is_entity_bound();
    let mut entities = Vec::new();
    let mut cells: Cells<'_> = fields
        .iter()
        .map(|field| (field.as_str(), Vec::new()))
        .collect();
    for row in rows {
        let Row::Fact {
            entity,
            cells: values,
            ..
        } = row
        else {
            continue;
        };
        if entity_bound {
            entities.push(json!(
                entity.as_ref().map(Security::symbol).unwrap_or_default()
            ));
        }
        for field in fields {
            let cell = values
                .get(field.as_str())
                .cloned()
                .map_or(Value::Null, FactValue::json);
            cells.get_mut(field.as_str()).unwrap().push(cell);
        }
    }
    let mut out = envelope("table", dataset, meta);
    if entity_bound {
        out.insert("entities".to_owned(), Value::Array(entities));
    }
    out.insert(
        "versions".to_owned(),
        Value::Array(version_rows(manifest, &overrides)),
    );
    if !shared.is_empty() {
        out.insert("metadata".to_owned(), Value::Object(shared));
    }
    out.insert("columns".to_owned(), Value::Object(columns(fields, &cells)));
    out.insert("row_count".to_owned(), json!(rows.len()));
    Value::Object(out)
}

/// `layout:"table"` for `list_data_versions`: one row per version, no
/// `integrity_sha256` (it equals `version_id`).
pub fn versions_table(dataset: Dataset, items: &[VersionInfo], meta: &PageMeta) -> Value {
    let versions = items
        .iter()
        .map(|item| {
            let mut row = Map::new();
            if let Some(entity) = &item.version_ref.entity {
                row.insert("entity".to_owned(), json!(entity.symbol()));
            }
            row.insert("version_id".to_owned(), json!(item.version_ref.version_id));
            row.insert(
                "published_at".to_owned(),
                serde_json::to_value(item.published_at).unwrap(),
            );
            Value::Object(row)
        })
        .collect::<Vec<_>>();
    let mut out = envelope("table", dataset, meta);
    out.insert("versions".to_owned(), Value::Array(versions));
    out.insert("row_count".to_owned(), json!(items.len()));
    Value::Object(out)
}

fn availability_cell(item: &AvailabilityItem, field: &str) -> Value {
    // A universe row is already collapsed; per-task columns would only repeat
    // zeros for every member and break the one-row budget (v8 #387 §4.3).
    if item.universe.is_some() && matches!(field, "related_tasks_total" | "related_tasks_truncated")
    {
        return Value::Null;
    }
    match field {
        "availability" => json!(item.availability),
        "integrity" => json!(item.integrity),
        "version_id" => item
            .version_ref
            .as_ref()
            .map_or(Value::Null, |reference| json!(reference.version_id)),
        "coverage_evidence" => json!(item.coverage_evidence),
        "first_record" => item
            .first_record
            .map_or(Value::Null, |day| json!(day.to_string())),
        "last_record" => item
            .last_record
            .map_or(Value::Null, |day| json!(day.to_string())),
        "record_count" => json!(item.record_count),
        "freshness" => json!(item.freshness),
        "stale" => item
            .stale
            .as_ref()
            .map_or(Value::Null, |stale| json!(stale)),
        "as_of_day" => item
            .as_of_day
            .map_or(Value::Null, |day| json!(day.to_string())),
        "reason" => item
            .reason
            .map_or(Value::Null, |reason| serde_json::to_value(reason).unwrap()),
        "confirmed_gaps" => serde_json::to_value(&item.confirmed_gaps).unwrap(),
        "unresolved_ranges" => serde_json::to_value(&item.unresolved_ranges).unwrap(),
        "limitations" => serde_json::to_value(&item.limitations).unwrap(),
        "related_tasks" => Value::Array(
            item.related_tasks
                .iter()
                .map(|task| json!(task.task_id))
                .collect(),
        ),
        "related_tasks_total" => json!(item.related_tasks_total),
        "related_tasks_truncated" => json!(item.related_tasks_truncated),
        "universe" => item
            .universe
            .as_ref()
            .map_or(Value::Null, |summary| json!(summary)),
        _ => Value::Null,
    }
}

/// `layout:"table"` for `get_data_availability`: one row per entity, the
/// related-task objects shared once under `tasks` (they repeat for every
/// entity; the per-row column keeps only the ids).
pub fn availability_table(
    dataset: Dataset,
    items: &[AvailabilityItem],
    fields: &[String],
    meta: &PageMeta,
) -> Value {
    let mut entities = Vec::new();
    let entity_bound = items.iter().all(|item| item.entity.is_some());
    let mut tasks = Map::new();
    let mut cells: Cells<'_> = fields
        .iter()
        .map(|field| (field.as_str(), Vec::new()))
        .collect();
    let mut limitations_shared = true;
    let mut first_limitations: Option<&[String]> = None;
    for item in items {
        if entity_bound {
            entities.push(json!(item.entity.as_ref().unwrap().symbol()));
        }
        for task in &item.related_tasks {
            if !tasks.contains_key(&task.task_id) {
                tasks.insert(task.task_id.clone(), serde_json::to_value(task).unwrap());
            }
        }
        match first_limitations {
            None => first_limitations = Some(item.limitations.as_slice()),
            Some(first) if first != item.limitations.as_slice() => limitations_shared = false,
            Some(_) => {}
        }
        for field in fields {
            cells
                .get_mut(field.as_str())
                .unwrap()
                .push(availability_cell(item, field));
        }
    }
    let hoist_limitations = limitations_shared
        && fields.iter().any(|field| field == "limitations")
        && first_limitations.is_some_and(|limitations| !limitations.is_empty());
    if hoist_limitations {
        for field in fields.iter().filter(|field| *field == "limitations") {
            cells.remove(field.as_str());
        }
    }
    let mut out = envelope("table", dataset, meta);
    if entity_bound {
        out.insert("entities".to_owned(), Value::Array(entities));
    }
    if !tasks.is_empty() {
        out.insert("tasks".to_owned(), Value::Object(tasks));
    }
    if hoist_limitations {
        out.insert(
            "limitations".to_owned(),
            serde_json::to_value(first_limitations.unwrap()).unwrap(),
        );
    }
    out.insert("columns".to_owned(), Value::Object(columns(fields, &cells)));
    out.insert("row_count".to_owned(), json!(items.len()));
    Value::Object(out)
}

/// `layout:"cross_section"`: `trade_days` once, `entities` once, and
/// `columns[field][day_index][entity_index]`. Defined here (PR-3 shape) and
/// reachable through `query_data` once PR-4 adds `selection.kind=universe`
/// and `filter.kind=cross_section`.
pub fn cross_section(
    dataset: Dataset,
    manifest: &[ManifestEntry],
    trade_days: &[NaiveDate],
    entities: &[Security],
    rows: &[Row],
    fields: &[String],
    meta: &PageMeta,
) -> Value {
    let (shared, overrides) = split_metadata(manifest);
    let mut by_key: BTreeMap<(String, NaiveDate), &Row> = BTreeMap::new();
    for row in rows {
        if let Row::Market { entity, date, .. } = row {
            by_key.insert((entity.symbol(), *date), row);
        }
    }
    let mut columns = Map::new();
    for field in fields {
        let mut per_day: Vec<Value> = Vec::with_capacity(trade_days.len());
        let mut all_absent = true;
        for day in trade_days {
            let mut cells = Vec::with_capacity(entities.len());
            for entity in entities {
                let cell = by_key
                    .get(&(entity.symbol(), *day))
                    .map_or(Value::Null, |row| market_cell(row, field));
                if !is_absent(&cell) {
                    all_absent = false;
                }
                cells.push(cell);
            }
            per_day.push(Value::Array(cells));
        }
        if !all_absent {
            columns.insert(field.clone(), Value::Array(per_day));
        }
    }
    let mut out = envelope("cross_section", dataset, meta);
    out.insert(
        "trade_days".to_owned(),
        Value::Array(
            trade_days
                .iter()
                .map(|day| json!(day.to_string()))
                .collect(),
        ),
    );
    out.insert(
        "entities".to_owned(),
        Value::Array(
            entities
                .iter()
                .map(|entity| json!(entity.symbol()))
                .collect(),
        ),
    );
    out.insert(
        "versions".to_owned(),
        Value::Array(version_rows(manifest, &overrides)),
    );
    if !shared.is_empty() {
        out.insert("metadata".to_owned(), Value::Object(shared));
    }
    out.insert("columns".to_owned(), Value::Object(columns));
    out.insert("row_count".to_owned(), json!(rows.len()));
    Value::Object(out)
}
