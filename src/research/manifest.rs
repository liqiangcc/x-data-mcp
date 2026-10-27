//! Snapshot manifest and identity (§7.3).
//!
//! The manifest is the only description of an export directory: there is no
//! README (§7.2), and a directory without `manifest.json` is not a snapshot.
//!
//! Identity rule (§7.3, AC-M1-7): `snapshot_id = "snapshot-" +
//! sha256(canonical_json(identity))[..32]`, where `identity` is
//! `{schema, request, catalog, generations, algorithms, series, derived}`. It
//! deliberately excludes `created_at` and `files`, so the same inputs produce
//! the same id and file bytes/compression cannot change the identity. The
//! canonical form is `serde_json_canonicalizer`, the same serializer the
//! evidence store uses.
//!
//! A selection may span source generations (M1 fix B). The identity carries
//! the distinct `generations` and, on every series/derived entry, the
//! `source_generation` that row came from, so the mix is explicit instead of
//! being rejected. Nothing run-dependent (start/finish time, WAL growth)
//! enters `manifest.json`: the whole file is byte-reproducible for the same
//! database and output options.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::{
    CatalogBinding, DerivedDataset, DerivedEntry, Error, ExportRequest, FilePlan, FileStat,
    GenerationBinding, SeriesEntry,
};

/// `manifest.json.schema` (§7.3).
pub const SCHEMA: &str = "x-data-mcp.research-snapshot.v1";

/// Prefix of every snapshot id (§7.3).
pub const ID_PREFIX: &str = "snapshot-";

/// Algorithm labels the identity carries (§7.3, §4.4). They are the stored
/// values, not copies: a renamed algorithm must rename the constant.
pub fn algorithms() -> Value {
    json!({
        "series_digest": crate::sqlite_market::series::DIGEST_FORMAT,
        "qfq": crate::qfq::ALGORITHM_VERSION,
        "factor_normalize": crate::sqlite_market::derived::ADJUSTMENT_EVENTS_ALGORITHM,
        "calendar": crate::trading_calendar::ALGORITHM_VERSION,
    })
}

fn request_value(request: &ExportRequest) -> Value {
    let entities = match &request.entities {
        super::EntitySelection::CatalogHead => json!("catalog"),
        super::EntitySelection::CatalogVersion(id) => json!({"catalog_version": id}),
        super::EntitySelection::Explicit(list) => json!(list),
    };
    json!({
        "dataset": request.dataset.as_str(),
        "adjustment": request.adjustment.as_str(),
        "start": request.start.map(|day| day.to_string()),
        "end": request.end.map(|day| day.to_string()),
        "entities": entities,
    })
}

fn source_generation(
    source_binding_digest: &str,
    lineage: &BTreeMap<String, GenerationBinding>,
) -> Result<Value, Error> {
    let generation = lineage.get(source_binding_digest).ok_or_else(|| {
        Error::Integrity(format!(
            "source binding {source_binding_digest} was not resolved while planning"
        ))
    })?;
    Ok(json!({
        "generation_id": generation.generation_id,
        "manifest_sha256": generation.manifest_sha256,
    }))
}

fn series_value(
    entry: &SeriesEntry,
    lineage: &BTreeMap<String, GenerationBinding>,
) -> Result<Value, Error> {
    Ok(json!({
        "entity": entry.entity,
        "period": "day",
        "adjustment": entry.adjustment.as_str(),
        "instrument_class": entry.instrument_class_name(),
        "price_scale": entry.price_scale,
        "version_id": entry.version_id,
        "record_count": entry.record_count,
        "first_day": entry.first_day.to_string(),
        "last_day": entry.last_day.to_string(),
        "source_binding_digest": entry.source_binding_digest,
        "source_generation": source_generation(&entry.source_binding_digest, lineage)?,
        "coverage": entry.coverage_name(),
        // Index-only in effect: a stock series is never filtered and reports 0.
        "excluded_bar_count": entry.index.as_ref().map_or(0, |i| i.excluded_bar_count),
    }))
}

fn derived_value(
    entry: &DerivedEntry,
    lineage: &BTreeMap<String, GenerationBinding>,
) -> Result<Value, Error> {
    let mut value = json!({
        "dataset": entry.dataset.as_str(),
        "version_id": entry.version_id,
        "record_count": entry.record_count,
        "first_day": entry.first_day.to_string(),
        "last_day": entry.last_day.to_string(),
        "algorithm_version": entry.algorithm_version,
        "digest_format": entry.digest_format,
        "source_generation": source_generation(&entry.source_binding_digest, lineage)?,
    });
    if entry.dataset == DerivedDataset::AdjustmentEvents {
        value["entity"] = json!(entry.key);
    }
    Ok(value)
}

/// The identity document (§7.3). Its canonical JSON is what `snapshot_id`
/// hashes, so nothing time- or output-dependent may enter it.
pub fn identity(
    request: &ExportRequest,
    catalog: &CatalogBinding,
    generations: &[GenerationBinding],
    lineage: &BTreeMap<String, GenerationBinding>,
    series: &[SeriesEntry],
    derived: &[DerivedEntry],
) -> Result<Value, Error> {
    let series = series
        .iter()
        .map(|entry| series_value(entry, lineage))
        .collect::<Result<Vec<_>, _>>()?;
    let derived = derived
        .iter()
        .map(|entry| derived_value(entry, lineage))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(json!({
        "schema": SCHEMA,
        "request": request_value(request),
        "catalog": {
            "selection": catalog.selection,
            "version_id": catalog.version_id,
            "member_count": catalog.member_count,
        },
        "generations": generations
            .iter()
            .map(|generation| json!({
                "generation_id": generation.generation_id,
                "manifest_sha256": generation.manifest_sha256,
            }))
            .collect::<Vec<_>>(),
        "algorithms": algorithms(),
        "series": series,
        "derived": derived,
    }))
}

/// `snapshot-<32 hex>` of the canonical identity (§7.3, AC-M1-7).
pub fn snapshot_id(identity: &Value) -> Result<String, Error> {
    let canonical = serde_json_canonicalizer::to_string(identity)
        .map_err(|_| Error::Integrity("snapshot identity is not canonicalizable".into()))?;
    let digest = crate::hash::sha256(canonical.as_bytes());
    Ok(format!("{ID_PREFIX}{}", &digest[..32]))
}

/// The `files` array (§7.3, layer 4 of the binding). `stats` is `None` for
/// `--dry-run`, where rows/bytes/sha256 are not known because nothing was
/// written; the array is excluded from the identity either way.
pub fn files_value(plans: &[FilePlan], stats: Option<&[FileStat]>, compression: &str) -> Value {
    Value::Array(
        plans
            .iter()
            .map(|plan| {
                let stat = stats.and_then(|list| list.iter().find(|s| s.path == plan.path));
                json!({
                    "path": plan.path,
                    "rows": stat.map(|s| s.rows),
                    "bytes": stat.map(|s| s.bytes),
                    "sha256": stat.map(|s| s.sha256.clone()),
                    "compression": compression,
                    "columns": plan.columns,
                })
            })
            .collect(),
    )
}

/// The full `manifest.json` document: identity + output facts (§7.3).
///
/// `snapshot_id` must be the [`snapshot_id`] of `identity`; every caller takes
/// it from the same plan the identity came from, and the export test re-hashes
/// the written manifest, so the document cannot name an id the identity does
/// not hash to. Run-dependent facts (start/finish time, WAL growth) are **not**
/// part of this document: they stay in [`super::export::ExportOutcome`], so two
/// exports of the same inputs write byte-identical snapshots (AC-M1-7).
pub fn document(identity: &Value, snapshot_id: &str, files: Value, export: Value) -> Value {
    let mut document = identity.clone();
    document["snapshot_id"] = json!(snapshot_id);
    document["files"] = files;
    document["limitations"] = json!(super::LIMITATIONS);
    document["export"] = export;
    document
}
