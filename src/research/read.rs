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
            "SELECT {SERIES_COLUMNS},m.aliases,m.aggregate_scope,m.member_count \
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
        entry.index = Some(IndexBinding {
            aliases: raw.11,
            aggregate_scope: raw.12,
            member_count: raw.13,
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

fn read_source_binding(c: &Connection, digest: &str) -> Result<(String, String), Error> {
    let raw: Vec<u8> = decode_hex(digest).ok_or_else(|| integrity("binding digest is not hex"))?;
    let (generation_id, manifest): (String, Vec<u8>) = c
        .query_row(
            "SELECT generation_id,generation_manifest_sha256 FROM source_bindings \
             WHERE source_binding_digest=?1",
            params![raw],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => {
                Error::Integrity(format!("source binding {digest} is missing"))
            }
            other => db(other),
        })?;
    Ok((generation_id, digest_hex(manifest)?))
}

pub(crate) fn decode_hex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() / 2);
    for index in (0..text.len()).step_by(2) {
        out.push(u8::from_str_radix(&text[index..index + 2], 16).ok()?);
    }
    Some(out)
}

/// Layer 3 of the four-layer binding (§7.3, BR-M1-7): every exported row must
/// resolve to exactly one source generation. Two generations means the export
/// straddles a daily update — a half-new / half-old snapshot — so it fails
/// closed instead of writing one.
pub(crate) fn unique_generation(
    c: &Connection,
    series: &[SeriesEntry],
    derived: &[DerivedEntry],
) -> Result<GenerationBinding, Error> {
    let mut bindings: BTreeMap<String, (String, String)> = BTreeMap::new();
    let mut collect = |digest: &str| -> Result<(), Error> {
        let (generation_id, manifest_sha256) = read_source_binding(c, digest)?;
        bindings.insert(
            format!("{generation_id}/{manifest_sha256}"),
            (generation_id, manifest_sha256),
        );
        Ok(())
    };
    for entry in series {
        collect(&entry.source_binding_digest)?;
    }
    for entry in derived {
        collect(&entry.source_binding_digest)?;
    }
    match bindings.len() {
        0 => Err(Error::Integrity(
            "the snapshot binds no source generation".into(),
        )),
        1 => {
            let (generation_id, manifest_sha256) =
                bindings.into_values().next().expect("one binding");
            Ok(GenerationBinding {
                generation_id,
                manifest_sha256,
            })
        }
        _ => {
            let list: Vec<&str> = bindings.keys().map(String::as_str).collect();
            Err(Error::MixedGeneration(format!(
                "the selection spans {} source generations ({}); \
                 export one generation at a time",
                list.len(),
                list.join(",")
            )))
        }
    }
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
