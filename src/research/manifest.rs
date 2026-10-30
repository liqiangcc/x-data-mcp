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
    CatalogBinding, Coverage, DerivedDataset, DerivedEntry, Error, ExportRequest, FilePlan,
    FileStat, GenerationBinding, IndustryBinding, SeriesEntry,
};

/// `source_binding_digest` → generation. A named alias rather than the bare
/// `BTreeMap<..>` in four signatures: the generic's comma is not a second
/// parameter, and the pre-push arity heuristic reads one as such (the same
/// reason `security_master::ExcludedCategories` exists).
type LineageIndex = BTreeMap<String, GenerationBinding>;

/// `manifest.json.schema` (§7.3, #446 §10.4). v2 added
/// `files[].dataset`/`files[].temporal` (§12.4/§12.5); v3 adds the M3 industry
/// intervals and de-duplicated code migrations, the universe identity columns
/// (`canonical_entity`/`aliases`), the bound industry dataset version and the
/// `coverage` block. `snapshot_id` keeps its definition (canonical identity →
/// first 32 hex), and the schema string is part of the identity, so a v2
/// directory and a v3 directory with otherwise identical inputs differ (a v2
/// directory stays readable — see `scripts/export_smoke.py`).
pub const SCHEMA: &str = "x-data-mcp.research-snapshot.v3";

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

fn source_generation(source_binding_digest: &str, lineage: &LineageIndex) -> Result<Value, Error> {
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

fn series_value(entry: &SeriesEntry, lineage: &LineageIndex) -> Result<Value, Error> {
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

fn derived_value(entry: &DerivedEntry, lineage: &LineageIndex) -> Result<Value, Error> {
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

/// The v3 whole-market inputs the identity binds (#446 §10.4): the M3 industry
/// dataset identity and the `coverage` block. Grouped so `identity` keeps a
/// readable argument list (both are M3 facts, not top-level identity families).
pub struct WholeMarket<'a> {
    pub industry: Option<&'a IndustryBinding>,
    pub coverage: &'a Coverage,
}

/// The identity document (§7.3). Its canonical JSON is what `snapshot_id`
/// hashes, so nothing time- or output-dependent may enter it.
///
/// v3 adds two input facts (#446 §10.4): the M3 industry dataset identity (so a
/// database that differs only in industry rows cannot reuse a `snapshot_id`)
/// and the `coverage` block (`delisted_series`, `industry_coverage_start`). Both
/// are functions of the bound inputs, so the manifest stays byte-reproducible.
pub fn identity(
    request: &ExportRequest,
    catalog: &CatalogBinding,
    generations: &[GenerationBinding],
    lineage: &LineageIndex,
    series: &[SeriesEntry],
    derived: &[DerivedEntry],
    whole_market: &WholeMarket<'_>,
) -> Result<Value, Error> {
    let WholeMarket { industry, coverage } = whole_market;
    let industry = *industry;
    let series = series
        .iter()
        .map(|entry| series_value(entry, lineage))
        .collect::<Result<Vec<_>, _>>()?;
    let derived = derived
        .iter()
        .map(|entry| derived_value(entry, lineage))
        .collect::<Result<Vec<_>, _>>()?;
    let industry = industry.map_or(Value::Null, |binding| {
        json!({
            "dataset": super::INDUSTRY_DATASET,
            "version_id": binding.version_id,
            "snapshot_rows": binding.snapshot_rows,
            "assignment_rows": binding.assignment_rows,
        })
    });
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
        "coverage": {
            "delisted_series": coverage.delisted_series,
            "industry_coverage_start": coverage.industry_coverage_start.map(|day| day.to_string()),
        },
        "industry": industry,
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
                    "dataset": plan.dataset,
                    "temporal": plan.temporal.value(),
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
