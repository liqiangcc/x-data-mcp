//! Read side of the research snapshot: the read-only connection, the selection
//! queries that produce the snapshot identity, and the streaming row iterators
//! the Parquet writer consumes.
//!
//! Layer position (separation point 4): this file is the only place that knows
//! the physical layout of `market_series`, `series_bars`,
//! `derived_dataset_versions`, `adjustment_events`, `trading_days` and
//! `index_series_meta` for export purposes. It returns dataset rows
//! ([`SeriesEntry`], [`DerivedEntry`], [`BarRow`], [`FactorRow`]) so the writer
//! and the manifest never build SQL.
//!
//! Every bulk read streams: [`for_each_bar`] hands one row at a time to a
//! closure and never materializes a whole series, let alone the 16.7M-bar
//! universe. The selection queries are bounded by the number of series
//! (thousands of rows), never by the number of bars.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

use chrono::NaiveDate;
use rusqlite::{Connection, OpenFlags, params};
use rust_decimal::Decimal;
use rust_decimal::RoundingStrategy;

use super::{
    CatalogBinding, DerivedDataset, DerivedEntry, EntitySelection, ExportRequest,
    GenerationBinding, IndexBinding, SeriesEntry,
};
use crate::compact_market;
use crate::market_data::{
    Adjustment, CoverageStatus, INSTRUMENT_CLASS_INDEX, INSTRUMENT_CLASS_STOCK, Period,
};
use crate::research::Error;
use crate::sqlite_market::{compact_repo, derived};

fn integrity(message: impl std::fmt::Display) -> Error {
    Error::Integrity(message.to_string())
}

fn db(error: impl std::fmt::Display) -> Error {
    Error::Db(error.to_string())
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn digest_hex(raw: Vec<u8>) -> Result<String, Error> {
    if raw.len() != 32 {
        return Err(integrity("digest column is not 32 bytes"));
    }
    Ok(hex(&raw))
}

/// Read-only, `query_only` connection with the same pragmas the rest of the
/// process uses for read connections (`SqliteTasks::connect(true)`).
pub(crate) fn open_read_only(path: &Path) -> Result<Connection, Error> {
    let c = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| Error::NotFound(format!("{}: {e}", path.display())))?;
    c.busy_timeout(Duration::from_secs(5)).map_err(db)?;
    c.pragma_update(None, "foreign_keys", true).map_err(db)?;
    // Belt and braces next to SQLITE_OPEN_READ_ONLY: a write statement is
    // rejected by the connection itself, not only by the file layer (AC-M1-9).
    c.pragma_update(None, "query_only", true).map_err(db)?;
    let version: u32 = c
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(db)?;
    if version != crate::sqlite_tasks::COMPACT_SCHEMA_VERSION {
        return Err(Error::Schema(format!(
            "database is schema {version}; export requires {} (D-08 fresh init)",
            crate::sqlite_tasks::COMPACT_SCHEMA_VERSION
        )));
    }
    crate::sqlite_tasks::check_schema(&c, true).map_err(db)?;
    Ok(c)
}

/// The catalog the entity selection resolved to, plus the member set the stock
/// filter uses (`None` for an explicit list).
pub(crate) struct Catalog {
    pub binding: CatalogBinding,
    pub members: Option<BTreeSet<String>>,
}

pub(crate) fn catalog(c: &Connection, selection: &EntitySelection) -> Result<Catalog, Error> {
    let version_id = match selection {
        EntitySelection::CatalogHead => Some(
            crate::sqlite_catalog::head(c)
                .map_err(Error::from)?
                .ok_or_else(|| Error::NotFound("no catalog head is published".into()))?,
        ),
        EntitySelection::CatalogVersion(id) => {
            let exists: bool = c
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM catalog_versions WHERE version_id=?1)",
                    params![id],
                    |r| r.get(0),
                )
                .map_err(db)?;
            if !exists {
                return Err(Error::NotFound(format!(
                    "catalog version {id} does not exist"
                )));
            }
            Some(id.clone())
        }
        EntitySelection::Explicit(list) => {
            return Ok(Catalog {
                binding: CatalogBinding {
                    selection: selection.as_str(),
                    version_id: None,
                    member_count: list.len(),
                },
                members: None,
            });
        }
    };
    let version_id = version_id.expect("catalog selections always name a version");
    let mut statement = c
        .prepare(
            "SELECT exchange || '.' || code FROM catalog_members WHERE version_id=?1 ORDER BY 1",
        )
        .map_err(db)?;
    let members: BTreeSet<String> = statement
        .query_map(params![version_id], |r| r.get::<_, String>(0))
        .map_err(db)?
        .collect::<Result<_, _>>()
        .map_err(db)?;
    if members.is_empty() {
        return Err(Error::NotFound(format!(
            "catalog version {version_id} has no member"
        )));
    }
    Ok(Catalog {
        binding: CatalogBinding {
            selection: selection.as_str(),
            version_id: Some(version_id),
            member_count: members.len(),
        },
        members: Some(members),
    })
}

type RawSeries = (
    i64,
    String,
    i64,
    i64,
    i64,
    String,
    i64,
    i64,
    i64,
    i64,
    Vec<u8>,
);

const SERIES_COLUMNS: &str = "s.series_key,s.entity,s.adjustment,s.instrument_class,\
     s.price_scale,s.version_id,s.record_count,s.first_day,s.last_day,s.coverage_status,\
     s.source_binding_digest";

fn series_entry(raw: RawSeries) -> Result<SeriesEntry, Error> {
    Ok(SeriesEntry {
        series_key: raw.0,
        entity: raw.1,
        adjustment: Adjustment::from_code(raw.2)
            .ok_or_else(|| integrity("unknown series adjustment"))?,
        instrument_class: raw.3,
        price_scale: u32::try_from(raw.4).map_err(|_| integrity("negative price_scale"))?,
        version_id: raw.5,
        record_count: raw.6,
        first_day: compact_market::epoch_date(raw.7).map_err(integrity)?,
        last_day: compact_market::epoch_date(raw.8).map_err(integrity)?,
        coverage: match raw.9 {
            1 => CoverageStatus::Complete,
            2 => CoverageStatus::Incomplete,
            3 => CoverageStatus::Unknown,
            _ => return Err(integrity("unknown coverage_status")),
        },
        source_binding_digest: digest_hex(raw.10)?,
        index: None,
    })
}

fn period_day() -> i64 {
    compact_repo::period_code(Period::Day)
}

/// Stock daily series of the selection, 不复权 and/or 前复权 (§7.1).
///
/// An explicit entity list is validated against the published series: naming an
/// entity that has no daily series is a request error, not a silently smaller
/// export. Catalog selections need no such check — a member without a published
/// series is simply not part of the snapshot, and `catalog.member_count` says so.
pub(crate) fn stock_series(
    c: &Connection,
    request: &ExportRequest,
    members: Option<&BTreeSet<String>>,
) -> Result<Vec<SeriesEntry>, Error> {
    let explicit = request.entities.explicit();
    let mut statement = c
        .prepare(&format!(
            "SELECT {SERIES_COLUMNS} FROM market_series s \
             WHERE s.period=?1 AND s.instrument_class=?2 ORDER BY s.entity, s.adjustment"
        ))
        .map_err(db)?;
    let rows = statement
        .query_map(params![period_day(), INSTRUMENT_CLASS_STOCK], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, i64>(8)?,
                r.get::<_, i64>(9)?,
                r.get::<_, Vec<u8>>(10)?,
            ))
        })
        .map_err(db)?;
    let mut entries = Vec::new();
    let mut published: BTreeSet<String> = BTreeSet::new();
    for row in rows {
        let entry = series_entry(row.map_err(db)?)?;
        let selected = match (explicit, members) {
            (Some(list), _) => list.binary_search(&entry.entity).is_ok(),
            (None, Some(members)) => members.contains(&entry.entity),
            (None, None) => true,
        };
        if !selected {
            continue;
        }
        published.insert(entry.entity.clone());
        if request.adjustment.includes(entry.adjustment) {
            entries.push(entry);
        }
    }
    if let Some(list) = explicit {
        let missing: Vec<&str> = list
            .iter()
            .filter(|entity| !published.contains(*entity))
            .map(String::as_str)
            .collect();
        if !missing.is_empty() {
            return Err(Error::NotFound(format!(
                "no published stock daily series for {}",
                missing.join(",")
            )));
        }
    }
    Ok(entries)
}

/// Every registered index benchmark series (§6.3). Index series are always
/// 不复权 at `price_scale=2` and are never filtered by `--entities`.
pub(crate) fn index_series(c: &Connection) -> Result<Vec<SeriesEntry>, Error> {
    let mut statement = c
        .prepare(&format!(
            "SELECT {SERIES_COLUMNS},m.aliases,m.aggregate_scope,m.member_count,s.evidence_digest \
             FROM market_series s JOIN index_series_meta m ON m.series_key=s.series_key \
             WHERE s.period=?1 AND s.instrument_class=?2 AND s.adjustment=?3 \
             ORDER BY s.entity"
        ))
        .map_err(db)?;
    let rows = statement
        .query_map(
            params![period_day(), INSTRUMENT_CLASS_INDEX, Adjustment::Raw.code()],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, i64>(7)?,
                    r.get::<_, i64>(8)?,
                    r.get::<_, i64>(9)?,
                    r.get::<_, Vec<u8>>(10)?,
                    r.get::<_, String>(11)?,
                    r.get::<_, String>(12)?,
                    r.get::<_, Option<i64>>(13)?,
                    r.get::<_, Vec<u8>>(14)?,
                ))
            },
        )
        .map_err(db)?;
    let mut entries = Vec::new();
    for row in rows {
        let raw = row.map_err(db)?;
        let mut entry = series_entry((
            raw.0, raw.1, raw.2, raw.3, raw.4, raw.5, raw.6, raw.7, raw.8, raw.9, raw.10,
        ))?;
        let digest: [u8; 32] = raw
            .14
            .as_slice()
            .try_into()
            .map_err(|_| integrity("series evidence_digest is not 32 bytes"))?;
        let (kind, canonical) = crate::sqlite_market::evidence_store::read_evidence(c, &digest)?;
        if kind != crate::compact_market::EvidenceKind::Version {
            return Err(integrity("index series evidence kind mismatch"));
        }
        let evidence: crate::compact_market::VersionEvidence =
            serde_json::from_str(&canonical).map_err(integrity)?;
        entry.index = Some(IndexBinding {
            aliases: raw.11,
            aggregate_scope: raw.12,
            member_count: raw.13,
            excluded_bar_count: evidence.excluded_bars.as_ref().map_or(0, Vec::len),
        });
        entries.push(entry);
    }
    Ok(entries)
}

type RawDerived = (String, String, i64, i64, i64, String, String, Vec<u8>);

fn derived_entry(dataset: DerivedDataset, raw: RawDerived) -> Result<DerivedEntry, Error> {
    Ok(DerivedEntry {
        dataset,
        key: raw.0,
        version_id: raw.1,
        record_count: raw.2,
        first_day: compact_market::epoch_date(raw.3).map_err(integrity)?,
        last_day: compact_market::epoch_date(raw.4).map_err(integrity)?,
        digest_format: raw.5,
        algorithm_version: raw.6,
        source_binding_digest: digest_hex(raw.7)?,
    })
}

/// The factor sets of the selection (§3.6/§3.7). An entity whose verified event
/// set is empty has no registration row, so it simply does not appear.
pub(crate) fn factor_sets(
    c: &Connection,
    request: &ExportRequest,
    members: Option<&BTreeSet<String>>,
) -> Result<Vec<DerivedEntry>, Error> {
    let explicit = request.entities.explicit();
    let mut statement = c
        .prepare(
            "SELECT dataset_key,version_id,record_count,first_day,last_day,digest_format,\
             algorithm_version,source_binding_digest FROM derived_dataset_versions \
             WHERE dataset_kind=?1 ORDER BY dataset_key",
        )
        .map_err(db)?;
    let rows = statement
        .query_map(params![derived::ADJUSTMENT_EVENTS_KIND], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, Vec<u8>>(7)?,
            ))
        })
        .map_err(db)?;
    let mut entries = Vec::new();
    for row in rows {
        let entry = derived_entry(DerivedDataset::AdjustmentEvents, row.map_err(db)?)?;
        let selected = match (explicit, members) {
            (Some(list), _) => list.binary_search(&entry.key).is_ok(),
            (None, Some(members)) => members.contains(&entry.key),
            (None, None) => true,
        };
        if selected {
            entries.push(entry);
        }
    }
    Ok(entries)
}

/// The registered trading calendar (§3.8/§3.7).
pub(crate) fn calendar_version(c: &Connection) -> Result<DerivedEntry, Error> {
    let row = derived::read_version(
        c,
        crate::trading_calendar::DATASET_KIND,
        crate::trading_calendar::DATASET_KEY,
    )?
    .ok_or_else(|| Error::NotFound("no trading calendar is derived yet".into()))?;
    Ok(DerivedEntry {
        dataset: DerivedDataset::TradingCalendar,
        key: row.dataset_key,
        version_id: row.version_id,
        record_count: row.record_count,
        first_day: compact_market::epoch_date(row.first_day).map_err(integrity)?,
        last_day: compact_market::epoch_date(row.last_day).map_err(integrity)?,
        digest_format: row.digest_format,
        algorithm_version: row.algorithm_version,
        source_binding_digest: hex(row.source_binding_digest.as_slice()),
    })
}

/// The registration of one whole-market M2 dataset (§12.4), or `None` when the
/// dataset was never written (the file is then exported with 0 rows and the
/// identity carries no version for it — an unwritten dataset is not a version).
pub(crate) fn panel_version(
    c: &Connection,
    dataset: DerivedDataset,
    kind: &str,
    key: &str,
) -> Result<Option<DerivedEntry>, Error> {
    let Some(row) = derived::read_version(c, kind, key).map_err(Error::from)? else {
        return Ok(None);
    };
    Ok(Some(DerivedEntry {
        dataset,
        key: row.dataset_key,
        version_id: row.version_id,
        record_count: row.record_count,
        first_day: compact_market::epoch_date(row.first_day).map_err(integrity)?,
        last_day: compact_market::epoch_date(row.last_day).map_err(integrity)?,
        digest_format: row.digest_format,
        algorithm_version: row.algorithm_version,
        source_binding_digest: hex(row.source_binding_digest.as_slice()),
    }))
}

/// Layer 3 of the four-layer binding (§7.3, BR-M1-7): every exported row must
/// resolve to a source generation. The generations do **not** have to be the
/// same: a daily run can leave one entity on generation N and another on N+1,
/// and the manifest records the binding of each row, so the mix is traceable.
/// Only a row whose binding has no `source_bindings` row is rejected — then
/// its lineage cannot be proven at all (M1 fix B).
///
/// The resolved per-row lineage plus the distinct generations (sorted).
pub(crate) struct Lineage {
    pub by_digest: BTreeMap<String, GenerationBinding>,
    pub generations: Vec<GenerationBinding>,
}

pub(crate) fn resolve_lineage(
    c: &Connection,
    series: &[SeriesEntry],
    derived: &[DerivedEntry],
) -> Result<Lineage, Error> {
    let mut statement = c
        .prepare(
            "SELECT source_binding_digest,generation_id,generation_manifest_sha256 \
             FROM source_bindings",
        )
        .map_err(db)?;
    let rows = statement
        .query_map([], |r| {
            Ok((
                r.get::<_, Vec<u8>>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Vec<u8>>(2)?,
            ))
        })
        .map_err(db)?;
    let mut known: BTreeMap<String, GenerationBinding> = BTreeMap::new();
    for row in rows {
        let (digest, generation_id, manifest) = row.map_err(db)?;
        known.insert(
            digest_hex(digest)?,
            GenerationBinding {
                generation_id,
                manifest_sha256: digest_hex(manifest)?,
            },
        );
    }

    let mut referenced: BTreeMap<String, GenerationBinding> = BTreeMap::new();
    let mut collect = |digest: &str| -> Result<(), Error> {
        let generation = known.get(digest).ok_or_else(|| {
            Error::UnprovenLineage(format!(
                "source binding {digest} has no source_bindings row; \
                 the row's source generation cannot be proven"
            ))
        })?;
        referenced.insert(digest.to_string(), generation.clone());
        Ok(())
    };
    for entry in series {
        collect(&entry.source_binding_digest)?;
    }
    for entry in derived {
        collect(&entry.source_binding_digest)?;
    }
    if referenced.is_empty() {
        return Err(Error::Integrity(
            "the snapshot binds no source generation".into(),
        ));
    }

    let mut generations: Vec<GenerationBinding> = referenced.values().cloned().collect();
    generations.sort_by(|a, b| {
        a.generation_id
            .cmp(&b.generation_id)
            .then_with(|| a.manifest_sha256.cmp(&b.manifest_sha256))
    });
    generations.dedup();
    Ok(Lineage {
        by_digest: referenced,
        generations,
    })
}

/// One `series_bars` row, decoded for export.
#[derive(Debug, Clone, PartialEq)]
pub struct BarRow {
    pub trade_day: NaiveDate,
    pub open_units: i64,
    pub high_units: i64,
    pub low_units: i64,
    pub close_units: i64,
    pub volume: Option<i64>,
    pub amount: Option<Decimal>,
    pub r7_raw: Option<u32>,
    pub volume_quantum: Option<i64>,
}

/// Stream one series' bars inside `[start, end]` in `trade_day` order.
///
/// Returns the number of rows handed to `f`. The row count is what the caller
/// checks against the series' `record_count`, so a series row that claims more
/// bars than the table holds fails closed instead of exporting a truncated
/// file that still carries the claimed version id.
pub fn for_each_bar<F>(
    c: &Connection,
    series_key: i64,
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
    mut f: F,
) -> Result<i64, Error>
where
    F: FnMut(BarRow) -> Result<(), Error>,
{
    let mut statement = c
        .prepare(
            "SELECT trade_day,open_units,high_units,low_units,close_units,volume,amount,\
             r7_raw,volume_quantum FROM series_bars WHERE series_key=?1 \
             AND trade_day>=COALESCE(?2,-9223372036854775808) \
             AND trade_day<=COALESCE(?3,9223372036854775807) ORDER BY trade_day",
        )
        .map_err(db)?;
    let rows = statement
        .query_map(
            params![
                series_key,
                start.map(compact_market::epoch_day),
                end.map(compact_market::epoch_day)
            ],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, Option<i64>>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, Option<i64>>(7)?,
                    r.get::<_, Option<i64>>(8)?,
                ))
            },
        )
        .map_err(db)?;
    let mut count = 0i64;
    for row in rows {
        let raw = row.map_err(db)?;
        let amount = match raw.6 {
            Some(text) => Some(
                text.parse::<Decimal>()
                    .map_err(|_| integrity("stored amount is not a decimal"))?,
            ),
            None => None,
        };
        let r7_raw = match raw.7 {
            Some(value) => {
                Some(u32::try_from(value).map_err(|_| integrity("r7_raw out of range"))?)
            }
            None => None,
        };
        f(BarRow {
            trade_day: compact_market::epoch_date(raw.0).map_err(integrity)?,
            open_units: raw.1,
            high_units: raw.2,
            low_units: raw.3,
            close_units: raw.4,
            volume: raw.5,
            amount,
            r7_raw,
            volume_quantum: raw.8,
        })?;
        count += 1;
    }
    Ok(count)
}

/// One `adjustment_events` row plus the single-event affine the factor file
/// carries (`gbbq-cat1-2dp-v1` inputs → `event_transform`).
#[derive(Debug, Clone, PartialEq)]
pub struct FactorRow {
    pub entity: String,
    pub ordinal: i64,
    pub effective_day: NaiveDate,
    /// Always `None` in M1: the source has no announcement date (BR-M1-9).
    pub known_at: Option<NaiveDate>,
    pub category: i64,
    pub c: [Decimal; 4],
    pub factor_a: Decimal,
    pub factor_b: Decimal,
}

/// Stream one entity's factor events inside `[start, end]` by `ordinal`.
pub fn for_each_factor<F>(
    c: &Connection,
    entity: &str,
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
    mut f: F,
) -> Result<i64, Error>
where
    F: FnMut(FactorRow) -> Result<(), Error>,
{
    let mut statement = c
        .prepare(
            "SELECT ordinal,effective_day,known_at,category,c1,c2,c3,c4 FROM adjustment_events \
             WHERE entity=?1 AND effective_day>=COALESCE(?2,-9223372036854775808) \
             AND effective_day<=COALESCE(?3,9223372036854775807) ORDER BY ordinal",
        )
        .map_err(db)?;
    let rows = statement
        .query_map(
            params![
                entity,
                start.map(compact_market::epoch_day),
                end.map(compact_market::epoch_day)
            ],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, String>(7)?,
                ))
            },
        )
        .map_err(db)?;
    let mut count = 0i64;
    for row in rows {
        let raw = row.map_err(db)?;
        let known_at = match raw.2 {
            Some(text) => Some(
                NaiveDate::parse_from_str(&text, "%Y-%m-%d")
                    .map_err(|_| integrity("stored known_at is not a date"))?,
            ),
            None => None,
        };
        let mut c_values = [Decimal::ZERO; 4];
        for (slot, text) in c_values.iter_mut().zip([raw.4, raw.5, raw.6, raw.7]) {
            *slot = text
                .parse::<Decimal>()
                .map_err(|_| integrity("stored factor is not a decimal"))?;
        }
        let affine = crate::gbbq::event_transform(&c_values);
        f(FactorRow {
            entity: entity.to_string(),
            ordinal: raw.0,
            effective_day: compact_market::epoch_date(raw.1).map_err(integrity)?,
            known_at,
            category: raw.3,
            c: c_values,
            factor_a: affine.a,
            factor_b: affine.b,
        })?;
        count += 1;
    }
    Ok(count)
}

/// Stream the derived trading calendar inside `[start, end]`.
pub fn for_each_calendar_day<F>(
    c: &Connection,
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
    mut f: F,
) -> Result<i64, Error>
where
    F: FnMut(NaiveDate, i64) -> Result<(), Error>,
{
    let mut statement = c
        .prepare(
            "SELECT trade_day,sources FROM trading_days \
             WHERE trade_day>=COALESCE(?1,-9223372036854775808) \
             AND trade_day<=COALESCE(?2,9223372036854775807) ORDER BY trade_day",
        )
        .map_err(db)?;
    let rows = statement
        .query_map(
            params![
                start.map(compact_market::epoch_day),
                end.map(compact_market::epoch_day)
            ],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
        )
        .map_err(db)?;
    let mut count = 0i64;
    for row in rows {
        let (day, sources) = row.map_err(db)?;
        f(compact_market::epoch_date(day).map_err(integrity)?, sources)?;
        count += 1;
    }
    Ok(count)
}

/// Exact unscaled integer of `value` at `scale` decimal places.
///
/// Used for the Parquet `DECIMAL` columns. The rounding happens exactly once,
/// here, with the project's HALF_UP money convention; the caller never rounds.
/// A value whose unscaled form does not fit the column fails closed.
pub fn scaled_units(value: Decimal, scale: u32) -> Result<i128, Error> {
    let rounded = value.round_dp_with_strategy(scale, RoundingStrategy::MidpointAwayFromZero);
    let value_scale = rounded.scale();
    let mantissa = rounded.mantissa();
    if value_scale > scale {
        return Err(integrity("decimal scale exceeds the column scale"));
    }
    let factor = 10i128
        .checked_pow(scale - value_scale)
        .ok_or_else(|| integrity("decimal scale factor overflow"))?;
    mantissa
        .checked_mul(factor)
        .ok_or_else(|| integrity("decimal does not fit the column precision"))
}

/// Price units of a series at 6dp: `units * 10^(6 - price_scale)` (§7.4).
pub fn price_units(units: i64, price_scale: u32) -> Result<i64, Error> {
    if price_scale > 6 {
        return Err(integrity("price_scale above the DDL ceiling"));
    }
    let factor = 10i64
        .checked_pow(6 - price_scale)
        .ok_or_else(|| integrity("price scale factor overflow"))?;
    units
        .checked_mul(factor)
        .ok_or_else(|| integrity("price does not fit DECIMAL(18,6)"))
}

// ---------------------------------------------------------------------------
// M2 security-master rows (§12.4)
// ---------------------------------------------------------------------------

/// Interval rows are in range when the half-open interval `[from, to)`
/// intersects the request's inclusive `[start, end]` day window.
fn interval_in_range(
    from: NaiveDate,
    to: Option<NaiveDate>,
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
) -> bool {
    if end.is_some_and(|end| from > end) {
        return false;
    }
    if start.is_some_and(|start| to.is_some_and(|to| to <= start)) {
        return false;
    }
    true
}

/// Single-day rows (events, one-day intervals) use containment, the same
/// inclusive rule the bar files already use for `--start/--end`.
fn day_in_range(day: NaiveDate, start: Option<NaiveDate>, end: Option<NaiveDate>) -> bool {
    start.is_none_or(|start| day >= start) && end.is_none_or(|end| day <= end)
}

/// The `security_master` intervals of `universe_intervals`/`security_master`
/// (§12.4), ordered by row identity.
pub(crate) fn security_master_rows(
    c: &Connection,
    request: &ExportRequest,
) -> Result<Vec<crate::security_master::SecurityLifecycleFact>, Error> {
    let rows = crate::sqlite_market::security::read_security_master(c).map_err(Error::from)?;
    Ok(rows
        .into_iter()
        .filter(|row| {
            interval_in_range(
                row.effective_from,
                row.effective_to,
                request.start,
                request.end,
            )
        })
        .collect())
}

/// One `security_names` row (§12.4). The table has no writer yet, so the fields
/// stay in the stored vocabulary (see the storage decoder).
pub fn security_name_rows(
    c: &Connection,
    request: &ExportRequest,
) -> Result<Vec<crate::sqlite_market::security::SecurityNameRow>, Error> {
    let rows = crate::sqlite_market::security::read_security_names(c).map_err(Error::from)?;
    Ok(rows
        .into_iter()
        .filter(|row| {
            interval_in_range(
                row.effective_from,
                row.effective_to,
                request.start,
                request.end,
            )
        })
        .collect())
}

/// One capital event with the entity it belongs to (§12.4 `capital_events`).
pub fn capital_event_rows(
    c: &Connection,
    request: &ExportRequest,
) -> Result<Vec<(String, crate::security_master::CapitalEvent)>, Error> {
    let rows = crate::sqlite_market::security::read_all_capital_events(c).map_err(Error::from)?;
    Ok(rows
        .into_iter()
        .filter(|(_, event)| day_in_range(event.effective_day, request.start, request.end))
        .collect())
}

/// The risk-warning intervals (§12.4 `risk_warnings`).
pub fn risk_warning_rows(
    c: &Connection,
    request: &ExportRequest,
) -> Result<Vec<crate::security_master::RiskWarningInterval>, Error> {
    let rows =
        crate::sqlite_market::security::read_risk_warning_intervals(c).map_err(Error::from)?;
    Ok(rows
        .into_iter()
        .filter(|row| {
            interval_in_range(
                row.effective_from,
                row.effective_to,
                request.start,
                request.end,
            )
        })
        .collect())
}

/// The suspension intervals (§12.4 `suspensions`), confirmed and derived.
pub fn suspension_rows(
    c: &Connection,
    request: &ExportRequest,
) -> Result<Vec<crate::security_master::SuspensionInterval>, Error> {
    let rows = crate::sqlite_market::suspensions::read_suspensions(c).map_err(Error::from)?;
    Ok(rows
        .into_iter()
        .filter(|row| {
            interval_in_range(row.start_day, Some(row.end_day), request.start, request.end)
        })
        .collect())
}

/// The admitted and pending rule rows (§12.4 `price_limit_rules`). Both are
/// exported: `basis_status` is what tells a consumer which ones selected a
/// limit and which are provenance only (§8.1).
pub fn price_limit_rule_rows(
    c: &Connection,
    request: &ExportRequest,
) -> Result<Vec<crate::trading_rules::PriceLimitRule>, Error> {
    let book = crate::sqlite_market::rules::read_trading_rules(c).map_err(Error::from)?;
    Ok(book
        .price_limits
        .into_iter()
        .filter(|rule| {
            interval_in_range(
                rule.effective_from,
                rule.effective_to,
                request.start,
                request.end,
            )
        })
        .collect())
}

/// The member interval skeleton (§12.4 `universe_intervals`).
pub fn universe_interval_rows(
    c: &Connection,
    request: &ExportRequest,
) -> Result<Vec<super::universe::MemberInterval>, Error> {
    let rows = super::universe::member_intervals(c)?;
    Ok(rows
        .into_iter()
        .filter(|row| interval_in_range(row.member_from, row.member_to, request.start, request.end))
        .collect())
}
