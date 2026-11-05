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
use std::ops::Range;

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
pub const DAILY_FIELDS: &[&str] = &[
    "open", "high", "low", "close", "volume", "amount", "no_trade",
];
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
    "min_band_ticks",
    "band_side",
    "rounding",
    "effective_from",
    "effective_to",
    "basis_ref",
    "basis_status",
];
/// M3 PR-7 historical industry assignments (#446 §5.5). `status`/`reason`
/// make the `unknown` answer observable; the remaining fields are the
/// interval identity and its evidence.
pub const INDUSTRY_ASSIGNMENTS_FIELDS: &[&str] = &[
    "status",
    "reason",
    "scheme",
    "level",
    "industry_code",
    "name",
    "name_status",
    "effective_from",
    "effective_to",
    "known_at",
    "effective_from_basis",
    "evidence_grade",
    "source_snapshot_sha256",
];
pub const INDUSTRY_ASSIGNMENTS_DEFAULT: &[&str] = &[
    "status",
    "reason",
    "scheme",
    "level",
    "industry_code",
    "name",
    "name_status",
    "effective_from",
    "effective_to",
    "known_at",
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
        Dataset::IndustryAssignments => (INDUSTRY_ASSIGNMENTS_FIELDS, INDUSTRY_ASSIGNMENTS_DEFAULT),
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
    "verified_through",
    "price_scale",
    "last_trade_date",
    "delisting_date",
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
        // Dataset-level confirmation day (XDM-157): a `YYYY-MM-DD` string, the
        // same spelling as `as_of`/`known_at`, or absent when no complete
        // refresh run has confirmed the dataset's sources yet.
        "verified_through" => metadata
            .verified_through
            .map_or(Value::Null, |day| json!(day.to_string())),
        "price_scale" => metadata
            .price_scale
            .map_or(Value::Null, |scale| json!(scale)),
        "last_trade_date" => metadata
            .last_trade_date
            .map_or(Value::Null, |day| json!(day.to_string())),
        "delisting_date" => metadata
            .delisting_date
            .map_or(Value::Null, |day| json!(day.to_string())),
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
        } else if let Some(common) = common_fields(key, &values) {
            // An object that differs in some fields only: the fields every
            // entry agrees on are written once, each row keeps the rest.
            for (position, index) in present.iter().enumerate() {
                let rest: Map<String, Value> = values[position]
                    .as_object()
                    .into_iter()
                    .flatten()
                    .filter(|(field, _)| !common.contains_key(*field))
                    .map(|(field, value)| (field.clone(), value.clone()))
                    .collect();
                if !rest.is_empty() {
                    overrides[*index].insert((*key).to_owned(), Value::Object(rest));
                }
            }
            shared.insert((*key).to_owned(), Value::Object(common));
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

/// Metadata keys whose value is an object that can differ in a few fields only
/// (`adjustment.reason` states a per-entity event count while `field`,
/// `status` and `value` are the same for everyone). A reader lays a row's
/// object over the shared object of the same key, field by field.
const OBJECT_KEYS: &[&str] = &["adjustment", "lineage", "index"];

/// The fields every entry's object agrees on, for an object key whose every
/// value is an object; `None` when there is nothing to share.
fn common_fields(key: &str, values: &[Value]) -> Option<Map<String, Value>> {
    if !OBJECT_KEYS.contains(&key) || !values.iter().all(Value::is_object) {
        return None;
    }
    let common: Map<String, Value> = values[0]
        .as_object()?
        .iter()
        .filter(|(field, value)| {
            values[1..]
                .iter()
                .all(|other| other.get(field.as_str()) == Some(*value))
        })
        .map(|(field, value)| (field.clone(), value.clone()))
        .collect();
    (!common.is_empty()).then_some(common)
}

/// One `versions` row per manifest entry: the version list appears once and
/// `integrity_sha256` is gone (`version_id` is the digest, design §5.1 rule 2).
fn version_rows(manifest: &[ManifestEntry], overrides: &[Map<String, Value>]) -> Vec<Value> {
    version_rows_in(manifest, overrides, 0..manifest.len())
}

/// `version_rows` for the manifest entries in `span` only. `overrides` is the
/// full-manifest result of [`split_metadata`], so a page of a `universe` read
/// keeps the same shared `metadata` and the same per-entity rows as the whole
/// answer would have.
fn version_rows_in(
    manifest: &[ManifestEntry],
    overrides: &[Map<String, Value>],
    span: Range<usize>,
) -> Vec<Value> {
    manifest[span.clone()]
        .iter()
        .enumerate()
        .map(|(offset, entry)| {
            let index = span.start + offset;
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

/// Texts shorter than this stay where they are.
const TEXT_MIN_BYTES: usize = 48;
/// Bytes a moved text costs in its row (`_parts`, brackets, a table index).
const TEXT_REF_BYTES: usize = 16;
/// Texts are grouped by the last (first) characters before a common suffix
/// (prefix) is cut out of the group.
const TEXT_KEY_CHARS: usize = 24;

enum Part {
    Lit(String),
    Ref(usize),
}

#[derive(Clone, Copy)]
enum Place {
    CoverageReason,
    Adjustment,
    Unit(usize),
}

struct Slot {
    /// 0 is the shared `metadata`, `n` is `versions[n - 1]`.
    holder: usize,
    place: Place,
    /// Texts of one class are cut against each other.
    class: String,
    text: String,
    parts: Vec<Part>,
}

fn place_map(
    holder: &mut Map<String, Value>,
    place: Place,
) -> Option<(&mut Map<String, Value>, &'static str)> {
    match place {
        Place::CoverageReason => Some((holder, "coverage_reason")),
        Place::Adjustment => holder
            .get_mut("adjustment")?
            .as_object_mut()
            .map(|object| (object, "reason")),
        Place::Unit(index) => holder
            .get_mut("units")?
            .as_array_mut()?
            .get_mut(index)?
            .as_object_mut()
            .map(|object| (object, "reason")),
    }
}

fn holder_mut<'a>(
    shared: &'a mut Map<String, Value>,
    rows: &'a mut [Value],
    holder: usize,
) -> Option<&'a mut Map<String, Value>> {
    match holder {
        0 => Some(shared),
        row => rows.get_mut(row - 1)?.as_object_mut(),
    }
}

fn common_suffix_len(items: &[&str]) -> usize {
    let first = items[0];
    let mut len = first.len();
    for other in &items[1..] {
        len = len.min(
            first
                .bytes()
                .rev()
                .zip(other.bytes().rev())
                .take_while(|(a, b)| a == b)
                .count(),
        );
    }
    while len > 0 && !first.is_char_boundary(first.len() - len) {
        len -= 1;
    }
    len
}

fn common_prefix_len(items: &[&str]) -> usize {
    let first = items[0];
    let mut len = first.len();
    for other in &items[1..] {
        len = len.min(
            first
                .bytes()
                .zip(other.bytes())
                .take_while(|(a, b)| a == b)
                .count(),
        );
    }
    while len > 0 && !first.is_char_boundary(len) {
        len -= 1;
    }
    len
}

/// Move the long evidence texts (`coverage_reason`, `adjustment.reason`,
/// `units[].reason`) of the shared `metadata` and the `versions` rows into a
/// per-response table and return it. A text that several entries repeat is
/// stored once; texts of one kind that differ in a few characters (an event
/// count, a version id) are stored as a common prefix and suffix around the
/// literal middle. A moved text `reason` becomes `reason_parts` (and
/// `coverage_reason` becomes `coverage_reason_parts`): an array whose
/// numbers index the table and whose strings are literal; concatenating the
/// parts gives the original text. Nothing is dropped and the choice depends on
/// the content only, so equal input gives equal bytes.
fn compact_texts(shared: &mut Map<String, Value>, rows: &mut [Value]) -> Vec<String> {
    let mut slots: Vec<Slot> = Vec::new();
    for holder in 0..=rows.len() {
        let Some(map) = holder_mut(shared, rows, holder) else {
            continue;
        };
        let mut places = vec![(Place::CoverageReason, "coverage".to_owned())];
        if map.contains_key("adjustment") {
            places.push((Place::Adjustment, "adjustment".to_owned()));
        }
        for (index, unit) in map
            .get("units")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let field = unit.get("field").and_then(Value::as_str).unwrap_or("");
            places.push((Place::Unit(index), format!("unit:{field}")));
        }
        for (place, class) in places {
            let Some((target, key)) = place_map(map, place) else {
                continue;
            };
            let Some(text) = target.get(key).and_then(Value::as_str) else {
                continue;
            };
            if text.len() < TEXT_MIN_BYTES {
                continue;
            }
            slots.push(Slot {
                holder,
                place,
                class,
                text: text.to_owned(),
                parts: vec![Part::Lit(text.to_owned())],
            });
        }
    }

    let mut table: Vec<String> = Vec::new();

    // 1. A text several slots repeat is stored once.
    let mut same: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (index, slot) in slots.iter().enumerate() {
        same.entry(slot.text.as_str()).or_default().push(index);
    }
    let repeated: Vec<(String, Vec<usize>)> = same
        .into_iter()
        .filter(|(_, members)| members.len() >= 2)
        .map(|(text, members)| (text.to_owned(), members))
        .collect();
    for (text, members) in repeated {
        table.push(text);
        for index in members {
            slots[index].parts = vec![Part::Ref(table.len() - 1)];
        }
    }

    // 2. Texts of one kind that end alike share that ending, then the same
    //    for what they start with.
    for tail in [true, false] {
        let mut groups: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
        for (index, slot) in slots.iter().enumerate() {
            let edge = if tail {
                slot.parts.last()
            } else {
                slot.parts.first()
            };
            let Some(Part::Lit(text)) = edge else {
                continue;
            };
            if text.chars().count() < TEXT_KEY_CHARS {
                continue;
            }
            let key: String = if tail {
                let mut chars: Vec<char> = text.chars().rev().take(TEXT_KEY_CHARS).collect();
                chars.reverse();
                chars.into_iter().collect()
            } else {
                text.chars().take(TEXT_KEY_CHARS).collect()
            };
            groups
                .entry((slot.class.clone(), key))
                .or_default()
                .push(index);
        }
        for members in groups.into_values().filter(|members| members.len() >= 2) {
            let literals: Vec<&str> = members
                .iter()
                .map(|index| {
                    let edge = if tail {
                        slots[*index].parts.last()
                    } else {
                        slots[*index].parts.first()
                    };
                    match edge {
                        Some(Part::Lit(text)) => text.as_str(),
                        _ => "",
                    }
                })
                .collect();
            let len = if tail {
                common_suffix_len(&literals)
            } else {
                common_prefix_len(&literals)
            };
            if len < TEXT_MIN_BYTES
                || members.len() * (len - TEXT_REF_BYTES.min(len)) <= len + TEXT_REF_BYTES
            {
                continue;
            }
            let common = if tail {
                literals[0][literals[0].len() - len..].to_owned()
            } else {
                literals[0][..len].to_owned()
            };
            table.push(common);
            let reference = table.len() - 1;
            for index in members {
                let parts = &mut slots[index].parts;
                if tail {
                    let Some(Part::Lit(text)) = parts.pop() else {
                        continue;
                    };
                    let rest = text[..text.len() - len].to_owned();
                    if !rest.is_empty() {
                        parts.push(Part::Lit(rest));
                    }
                    parts.push(Part::Ref(reference));
                } else {
                    let Part::Lit(text) = parts.remove(0) else {
                        continue;
                    };
                    let rest = text[len..].to_owned();
                    if !rest.is_empty() {
                        parts.insert(0, Part::Lit(rest));
                    }
                    parts.insert(0, Part::Ref(reference));
                }
            }
        }
    }

    for slot in slots {
        if matches!(slot.parts.as_slice(), [Part::Lit(_)]) {
            continue;
        }
        let Some(map) = holder_mut(shared, rows, slot.holder) else {
            continue;
        };
        let Some((target, key)) = place_map(map, slot.place) else {
            continue;
        };
        let parts: Vec<Value> = slot
            .parts
            .into_iter()
            .map(|part| match part {
                Part::Lit(text) => json!(text),
                Part::Ref(index) => json!(index),
            })
            .collect();
        target.remove(key);
        target.insert(format!("{key}_parts"), Value::Array(parts));
    }
    table
}

/// `versions`, the shared `metadata` and the `texts` table of one answer.
fn put_versions(
    out: &mut Map<String, Value>,
    mut shared: Map<String, Value>,
    mut versions: Vec<Value>,
) {
    let texts = compact_texts(&mut shared, &mut versions);
    out.insert("versions".to_owned(), Value::Array(versions));
    if !shared.is_empty() {
        out.insert("metadata".to_owned(), Value::Object(shared));
    }
    if !texts.is_empty() {
        out.insert("texts".to_owned(), json!(texts));
    }
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
        no_trade,
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
        "no_trade" => no_trade.map_or(Value::Null, |value| json!(value)),
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
    panel_page(dataset, manifest, entities, rows, fields, meta, None)
}

/// [`panel`] for one page of a `selection.kind=universe` read. `span` is the
/// range of `entities` (and of `manifest`, which is aligned with it) this page
/// reports; `rows` are the page's rows only. The shared `metadata` is still
/// decided over the whole manifest, so every page of one query reports the
/// same split between `metadata` and per-entity `versions` rows. `None` keeps
/// every entity (the `single`/`batch` shape, unchanged byte for byte).
pub fn panel_page(
    dataset: Dataset,
    manifest: &[ManifestEntry],
    entities: &[Security],
    rows: &[Row],
    fields: &[String],
    meta: &PageMeta,
    span: Option<Range<usize>>,
) -> Value {
    let (shared, overrides) = split_metadata(manifest);
    let entities = span.clone().map_or(entities, |span| &entities[span]);
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
    put_versions(
        &mut out,
        shared,
        match span {
            Some(span) => version_rows_in(manifest, &overrides, span),
            None => version_rows(manifest, &overrides),
        },
    );
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
    put_versions(&mut out, shared, version_rows(manifest, &overrides));
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
    put_versions(&mut out, shared, version_rows(manifest, &overrides));
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
    put_versions(&mut out, shared, version_rows(manifest, &overrides));
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
    cross_section_page(
        dataset, manifest, trade_days, entities, rows, fields, meta, None,
    )
}

/// [`cross_section`] for one page of a `selection.kind=universe` read; `span`
/// has the same meaning as in [`panel_page`]. Cells of the reported entities
/// that belong to rows on another page stay `null`, as they did before.
#[allow(clippy::too_many_arguments)]
pub fn cross_section_page(
    dataset: Dataset,
    manifest: &[ManifestEntry],
    trade_days: &[NaiveDate],
    entities: &[Security],
    rows: &[Row],
    fields: &[String],
    meta: &PageMeta,
    span: Option<Range<usize>>,
) -> Value {
    let (shared, overrides) = split_metadata(manifest);
    let entities = span.clone().map_or(entities, |span| &entities[span]);
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
    put_versions(
        &mut out,
        shared,
        match span {
            Some(span) => version_rows_in(manifest, &overrides, span),
            None => version_rows(manifest, &overrides),
        },
    );
    out.insert("columns".to_owned(), Value::Object(columns));
    out.insert("row_count".to_owned(), json!(rows.len()));
    Value::Object(out)
}
