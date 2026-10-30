//! Parquet encoding for the research snapshot (issue-383 §7.4/§7.6, feature
//! `export`).
//!
//! Layer position (separation point 4): this file is the *output* format. It
//! consumes the read model's rows ([`super::read::BarRow`],
//! [`super::read::FactorRow`]) and never issues SQL, so the storage shape and
//! the file schema can change independently.
//!
//! Type mapping (§7.4):
//! - prices `DECIMAL(18,6)` on `INT64` — a 2dp or 6dp integer is exact at 6dp,
//!   so one column type covers both 口径 and the future 3dp ETF values;
//! - `amount` `DECIMAL(20,2)` and the factor affine `DECIMAL(28,18)` do not fit
//!   an `INT64` and are written as `FIXED_LEN_BYTE_ARRAY` (9 / 16 bytes,
//!   two's complement big-endian), the Parquet decimal encoding;
//! - `trade_day` is `DATE` (`INT32` epoch days), `price_scale` is
//!   `INTEGER(8,true)` (INT8), `r7_raw` is `INTEGER(32,false)` (UINT32).
//!
//! The writer uses the low-level `SerializedFileWriter` and not the Arrow
//! writer: §7.6 rules out the Arrow computation stack (≈ +5–8MB binary,
//! +2–4min CI) for what is a fixed-schema column dump.

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use parquet::basic::Compression as ParquetCompression;
use parquet::data_type::{
    BoolType, ByteArray, ByteArrayType, FixedLenByteArray, FixedLenByteArrayType, Int32Type,
    Int64Type,
};
use parquet::file::properties::WriterProperties;
use parquet::file::writer::SerializedFileWriter;
use parquet::schema::parser::parse_message_type;
use rusqlite::Connection;

use super::export::Compression;
use super::read::{self, BarRow, FactorRow};
use super::{
    CALENDAR_FILE, CAPITAL_EVENTS_COLUMNS, CAPITAL_EVENTS_FILE, CODE_MIGRATIONS_COLUMNS,
    CODE_MIGRATIONS_FILE, DAILY_QFQ_FILE, DAILY_RAW_FILE, Error, FACTORS_FILE, FileStat,
    INDEX_FILE, INDUSTRY_ASSIGNMENTS_COLUMNS, INDUSTRY_ASSIGNMENTS_FILE, PRICE_LIMIT_RULES_COLUMNS,
    PRICE_LIMIT_RULES_FILE, RISK_WARNINGS_COLUMNS, RISK_WARNINGS_FILE, SECURITY_MASTER_COLUMNS,
    SECURITY_MASTER_FILE, SECURITY_NAMES_COLUMNS, SECURITY_NAMES_FILE, SUSPENSIONS_COLUMNS,
    SUSPENSIONS_FILE, SeriesEntry, SnapshotPlan, UNIVERSE_INTERVALS_COLUMNS,
    UNIVERSE_INTERVALS_FILE,
};
use crate::market_data::{Adjustment, INSTRUMENT_CLASS_STOCK};

const DAILY_SCHEMA: &str = "
message daily {
  REQUIRED BINARY entity (STRING);
  REQUIRED INT32 trade_day (DATE);
  REQUIRED INT64 open (DECIMAL(18,6));
  REQUIRED INT64 high (DECIMAL(18,6));
  REQUIRED INT64 low (DECIMAL(18,6));
  REQUIRED INT64 close (DECIMAL(18,6));
  REQUIRED INT32 price_scale (INTEGER(8,true));
  OPTIONAL INT64 volume;
  OPTIONAL FIXED_LEN_BYTE_ARRAY(9) amount (DECIMAL(20,2));
  OPTIONAL INT32 r7_raw (INTEGER(32,false));
  OPTIONAL INT32 volume_quantum;
}";

const FACTOR_SCHEMA: &str = "
message factors {
  REQUIRED BINARY entity (STRING);
  REQUIRED INT32 ordinal;
  REQUIRED INT32 effective_day (DATE);
  OPTIONAL INT32 known_at (DATE);
  REQUIRED INT32 category (INTEGER(8,true));
  REQUIRED INT64 c1 (DECIMAL(18,2));
  REQUIRED INT64 c2 (DECIMAL(18,2));
  REQUIRED INT64 c3 (DECIMAL(18,2));
  REQUIRED INT64 c4 (DECIMAL(18,2));
  REQUIRED FIXED_LEN_BYTE_ARRAY(16) factor_a (DECIMAL(28,18));
  REQUIRED FIXED_LEN_BYTE_ARRAY(16) factor_b (DECIMAL(28,18));
}";

const CALENDAR_SCHEMA: &str = "
message calendar {
  REQUIRED INT32 trade_day (DATE);
  REQUIRED INT32 sources;
}";

const INDEX_SCHEMA: &str = "
message index_daily {
  REQUIRED BINARY entity (STRING);
  REQUIRED BINARY aliases (STRING);
  REQUIRED INT32 trade_day (DATE);
  REQUIRED INT64 open (DECIMAL(18,6));
  REQUIRED INT64 high (DECIMAL(18,6));
  REQUIRED INT64 low (DECIMAL(18,6));
  REQUIRED INT64 close (DECIMAL(18,6));
  REQUIRED INT32 price_scale (INTEGER(8,true));
  OPTIONAL INT64 volume;
  OPTIONAL FIXED_LEN_BYTE_ARRAY(9) amount (DECIMAL(20,2));
  REQUIRED BINARY aggregate_scope (STRING);
  OPTIONAL INT32 member_count;
}";

fn parquet_error(error: parquet::errors::ParquetError) -> Error {
    Error::Parquet(error.to_string())
}

fn properties(compression: Compression, row_group_rows: usize) -> WriterProperties {
    WriterProperties::builder()
        // Format 1.0 (the default) on purpose: a plain column dump needs no
        // 2.0 encoding, and 1.0 is what every reader implements.
        .set_compression(match compression {
            Compression::Snappy => ParquetCompression::SNAPPY,
            Compression::None => ParquetCompression::UNCOMPRESSED,
        })
        // Dictionary encoding is what §7.7's measured B/row assumes (the
        // crate default, stated here so a future property change is visible).
        .set_dictionary_enabled(true)
        // Recorded for readers that inspect the writer metadata; the actual
        // row groups are flushed by hand in `flush_*` (the low-level writer
        // does not split them itself).
        .set_max_row_group_row_count(Some(row_group_rows))
        // Constant, never the crate version: the file bytes must be
        // reproducible (AC-M1-7).
        .set_created_by("x-data-mcp research export".into())
        .build()
}

fn start(
    path: &Path,
    schema: &str,
    compression: Compression,
    rows: usize,
) -> Result<SerializedFileWriter<File>, Error> {
    let schema = parse_message_type(schema).map_err(parquet_error)?;
    let file = File::create(path).map_err(Error::from)?;
    SerializedFileWriter::new(
        file,
        Arc::new(schema),
        Arc::new(properties(compression, rows)),
    )
    .map_err(parquet_error)
}

fn sha256_file(path: &Path) -> Result<String, Error> {
    use sha2::{Digest as _, Sha256};
    let mut file = File::open(path).map_err(Error::from)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer).map_err(Error::from)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn file_stat(path: &Path, name: &str, rows: i64) -> Result<FileStat, Error> {
    let bytes = std::fs::metadata(path).map_err(Error::from)?.len();
    Ok(FileStat {
        path: name.to_string(),
        rows,
        bytes,
        sha256: sha256_file(path)?,
    })
}

/// A `DECIMAL(p,s)` value that does not fit `INT64` as a 9-byte fixed-length
/// two's complement integer (`DECIMAL(20,2)`).
fn fixed9(value: i128) -> Result<FixedLenByteArray, Error> {
    const LIMIT: i128 = 1 << 71;
    if !(-LIMIT..LIMIT).contains(&value) {
        return Err(Error::Integrity("amount does not fit DECIMAL(20,2)".into()));
    }
    Ok(FixedLenByteArray::from(ByteArray::from(
        value.to_be_bytes()[7..].to_vec(),
    )))
}

/// A `DECIMAL(28,18)` value as a 16-byte fixed-length two's complement integer.
fn fixed16(value: i128) -> FixedLenByteArray {
    FixedLenByteArray::from(ByteArray::from(value.to_be_bytes().to_vec()))
}

fn i64_value(value: i128) -> Result<i64, Error> {
    i64::try_from(value).map_err(|_| Error::Integrity("decimal does not fit INT64".into()))
}

fn i32_value(value: i64) -> Result<i32, Error> {
    i32::try_from(value).map_err(|_| Error::Integrity("day/value does not fit INT32".into()))
}

fn check_count(entry: &SeriesEntry, streamed: i64, plan: &SnapshotPlan) -> Result<(), Error> {
    if streamed > entry.record_count {
        return Err(Error::Integrity(format!(
            "series {} {} has {} bars but the table returned {streamed}",
            entry.entity,
            entry.adjustment.as_str(),
            entry.record_count
        )));
    }
    if plan.request.start.is_none() && plan.request.end.is_none() && streamed != entry.record_count
    {
        return Err(Error::Integrity(format!(
            "series {} {} claims {} bars but the table holds {streamed}",
            entry.entity,
            entry.adjustment.as_str(),
            entry.record_count
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// daily_raw.parquet / daily_qfq.parquet
// ---------------------------------------------------------------------------

#[derive(Default)]
struct DailyBatch {
    entity: Vec<ByteArray>,
    day: Vec<i32>,
    open: Vec<i64>,
    high: Vec<i64>,
    low: Vec<i64>,
    close: Vec<i64>,
    price_scale: Vec<i32>,
    volume: Vec<i64>,
    volume_def: Vec<i16>,
    amount: Vec<FixedLenByteArray>,
    amount_def: Vec<i16>,
    r7: Vec<i32>,
    r7_def: Vec<i16>,
    quantum: Vec<i32>,
    quantum_def: Vec<i16>,
    rows: usize,
}

impl DailyBatch {
    fn push(&mut self, entry: &SeriesEntry, row: BarRow) -> Result<(), Error> {
        self.entity
            .push(ByteArray::from(entry.entity.as_bytes().to_vec()));
        self.day.push(i32_value(compact_day(row.trade_day))?);
        let scale = entry.price_scale;
        self.open.push(read::price_units(row.open_units, scale)?);
        self.high.push(read::price_units(row.high_units, scale)?);
        self.low.push(read::price_units(row.low_units, scale)?);
        self.close.push(read::price_units(row.close_units, scale)?);
        self.price_scale.push(i32_value(i64::from(scale))?);
        match row.volume {
            Some(volume) => {
                self.volume.push(volume);
                self.volume_def.push(1);
            }
            None => self.volume_def.push(0),
        }
        match row.amount {
            Some(amount) => {
                self.amount.push(fixed9(read::scaled_units(amount, 2)?)?);
                self.amount_def.push(1);
            }
            None => self.amount_def.push(0),
        }
        match row.r7_raw {
            Some(value) => {
                // UINT32 bit pattern into the INT32 physical type.
                self.r7.push(value as i32);
                self.r7_def.push(1);
            }
            None => self.r7_def.push(0),
        }
        match row.volume_quantum {
            Some(value) => {
                self.quantum.push(i32_value(value)?);
                self.quantum_def.push(1);
            }
            None => self.quantum_def.push(0),
        }
        self.rows += 1;
        Ok(())
    }

    fn clear(&mut self) {
        self.entity.clear();
        self.day.clear();
        self.open.clear();
        self.high.clear();
        self.low.clear();
        self.close.clear();
        self.price_scale.clear();
        self.volume.clear();
        self.volume_def.clear();
        self.amount.clear();
        self.amount_def.clear();
        self.r7.clear();
        self.r7_def.clear();
        self.quantum.clear();
        self.quantum_def.clear();
        self.rows = 0;
    }
}

fn compact_day(day: chrono::NaiveDate) -> i64 {
    crate::compact_market::epoch_day(day)
}

fn flush_daily(
    writer: &mut SerializedFileWriter<File>,
    batch: &mut DailyBatch,
) -> Result<(), Error> {
    let mut group = writer.next_row_group().map_err(parquet_error)?;
    {
        let mut column = group
            .next_column()
            .map_err(parquet_error)?
            .ok_or_else(|| Error::Parquet("missing entity column".into()))?;
        column
            .typed::<ByteArrayType>()
            .write_batch(&batch.entity, None, None)
            .map_err(parquet_error)?;
        column.close().map_err(parquet_error)?;
    }
    {
        let mut column = group
            .next_column()
            .map_err(parquet_error)?
            .ok_or_else(|| Error::Parquet("missing trade_day column".into()))?;
        column
            .typed::<Int32Type>()
            .write_batch(&batch.day, None, None)
            .map_err(parquet_error)?;
        column.close().map_err(parquet_error)?;
    }
    for values in [&batch.open, &batch.high, &batch.low, &batch.close] {
        let mut column = group
            .next_column()
            .map_err(parquet_error)?
            .ok_or_else(|| Error::Parquet("missing price column".into()))?;
        column
            .typed::<Int64Type>()
            .write_batch(values, None, None)
            .map_err(parquet_error)?;
        column.close().map_err(parquet_error)?;
    }
    {
        let mut column = group
            .next_column()
            .map_err(parquet_error)?
            .ok_or_else(|| Error::Parquet("missing price_scale column".into()))?;
        column
            .typed::<Int32Type>()
            .write_batch(&batch.price_scale, None, None)
            .map_err(parquet_error)?;
        column.close().map_err(parquet_error)?;
    }
    {
        let mut column = group
            .next_column()
            .map_err(parquet_error)?
            .ok_or_else(|| Error::Parquet("missing volume column".into()))?;
        column
            .typed::<Int64Type>()
            .write_batch(&batch.volume, Some(&batch.volume_def), None)
            .map_err(parquet_error)?;
        column.close().map_err(parquet_error)?;
    }
    {
        let mut column = group
            .next_column()
            .map_err(parquet_error)?
            .ok_or_else(|| Error::Parquet("missing amount column".into()))?;
        column
            .typed::<FixedLenByteArrayType>()
            .write_batch(&batch.amount, Some(&batch.amount_def), None)
            .map_err(parquet_error)?;
        column.close().map_err(parquet_error)?;
    }
    {
        let mut column = group
            .next_column()
            .map_err(parquet_error)?
            .ok_or_else(|| Error::Parquet("missing r7_raw column".into()))?;
        column
            .typed::<Int32Type>()
            .write_batch(&batch.r7, Some(&batch.r7_def), None)
            .map_err(parquet_error)?;
        column.close().map_err(parquet_error)?;
    }
    {
        let mut column = group
            .next_column()
            .map_err(parquet_error)?
            .ok_or_else(|| Error::Parquet("missing volume_quantum column".into()))?;
        column
            .typed::<Int32Type>()
            .write_batch(&batch.quantum, Some(&batch.quantum_def), None)
            .map_err(parquet_error)?;
        column.close().map_err(parquet_error)?;
    }
    group.close().map_err(parquet_error)?;
    batch.clear();
    Ok(())
}

fn write_daily(
    path: &Path,
    name: &str,
    c: &Connection,
    plan: &SnapshotPlan,
    adjustment: Adjustment,
    compression: Compression,
    row_group_rows: usize,
) -> Result<FileStat, Error> {
    let mut writer = start(path, DAILY_SCHEMA, compression, row_group_rows)?;
    let mut batch = DailyBatch::default();
    let mut rows = 0i64;
    for entry in plan.series.iter().filter(|entry| {
        entry.instrument_class == INSTRUMENT_CLASS_STOCK && entry.adjustment == adjustment
    }) {
        let count = read::for_each_bar(
            c,
            entry.series_key,
            plan.request.start,
            plan.request.end,
            |row| {
                batch.push(entry, row)?;
                if batch.rows >= row_group_rows {
                    flush_daily(&mut writer, &mut batch)?;
                }
                Ok(())
            },
        )?;
        check_count(entry, count, plan)?;
        rows += count;
    }
    if batch.rows > 0 {
        flush_daily(&mut writer, &mut batch)?;
    }
    writer.close().map_err(parquet_error)?;
    file_stat(path, name, rows)
}

// ---------------------------------------------------------------------------
// factors.parquet
// ---------------------------------------------------------------------------

#[derive(Default)]
struct FactorBatch {
    entity: Vec<ByteArray>,
    ordinal: Vec<i32>,
    effective_day: Vec<i32>,
    known_at: Vec<i32>,
    known_at_def: Vec<i16>,
    category: Vec<i32>,
    c1: Vec<i64>,
    c2: Vec<i64>,
    c3: Vec<i64>,
    c4: Vec<i64>,
    factor_a: Vec<FixedLenByteArray>,
    factor_b: Vec<FixedLenByteArray>,
    rows: usize,
}

impl FactorBatch {
    fn push(&mut self, row: FactorRow) -> Result<(), Error> {
        self.entity
            .push(ByteArray::from(row.entity.as_bytes().to_vec()));
        self.ordinal.push(i32_value(row.ordinal)?);
        self.effective_day
            .push(i32_value(compact_day(row.effective_day))?);
        match row.known_at {
            Some(day) => {
                self.known_at.push(i32_value(compact_day(day))?);
                self.known_at_def.push(1);
            }
            None => self.known_at_def.push(0),
        }
        self.category.push(i32_value(row.category)?);
        self.c1.push(i64_value(read::scaled_units(row.c[0], 2)?)?);
        self.c2.push(i64_value(read::scaled_units(row.c[1], 2)?)?);
        self.c3.push(i64_value(read::scaled_units(row.c[2], 2)?)?);
        self.c4.push(i64_value(read::scaled_units(row.c[3], 2)?)?);
        self.factor_a
            .push(fixed16(read::scaled_units(row.factor_a, 18)?));
        self.factor_b
            .push(fixed16(read::scaled_units(row.factor_b, 18)?));
        self.rows += 1;
        Ok(())
    }

    fn clear(&mut self) {
        self.entity.clear();
        self.ordinal.clear();
        self.effective_day.clear();
        self.known_at.clear();
        self.known_at_def.clear();
        self.category.clear();
        self.c1.clear();
        self.c2.clear();
        self.c3.clear();
        self.c4.clear();
        self.factor_a.clear();
        self.factor_b.clear();
        self.rows = 0;
    }
}

fn flush_factors(
    writer: &mut SerializedFileWriter<File>,
    batch: &mut FactorBatch,
) -> Result<(), Error> {
    let mut group = writer.next_row_group().map_err(parquet_error)?;
    {
        let mut column = group
            .next_column()
            .map_err(parquet_error)?
            .ok_or_else(|| Error::Parquet("missing entity column".into()))?;
        column
            .typed::<ByteArrayType>()
            .write_batch(&batch.entity, None, None)
            .map_err(parquet_error)?;
        column.close().map_err(parquet_error)?;
    }
    for values in [&batch.ordinal, &batch.effective_day] {
        let mut column = group
            .next_column()
            .map_err(parquet_error)?
            .ok_or_else(|| Error::Parquet("missing factor int column".into()))?;
        column
            .typed::<Int32Type>()
            .write_batch(values, None, None)
            .map_err(parquet_error)?;
        column.close().map_err(parquet_error)?;
    }
    {
        let mut column = group
            .next_column()
            .map_err(parquet_error)?
            .ok_or_else(|| Error::Parquet("missing known_at column".into()))?;
        column
            .typed::<Int32Type>()
            .write_batch(&batch.known_at, Some(&batch.known_at_def), None)
            .map_err(parquet_error)?;
        column.close().map_err(parquet_error)?;
    }
    {
        let mut column = group
            .next_column()
            .map_err(parquet_error)?
            .ok_or_else(|| Error::Parquet("missing category column".into()))?;
        column
            .typed::<Int32Type>()
            .write_batch(&batch.category, None, None)
            .map_err(parquet_error)?;
        column.close().map_err(parquet_error)?;
    }
    for values in [&batch.c1, &batch.c2, &batch.c3, &batch.c4] {
        let mut column = group
            .next_column()
            .map_err(parquet_error)?
            .ok_or_else(|| Error::Parquet("missing factor decimal column".into()))?;
        column
            .typed::<Int64Type>()
            .write_batch(values, None, None)
            .map_err(parquet_error)?;
        column.close().map_err(parquet_error)?;
    }
    for values in [&batch.factor_a, &batch.factor_b] {
        let mut column = group
            .next_column()
            .map_err(parquet_error)?
            .ok_or_else(|| Error::Parquet("missing affine column".into()))?;
        column
            .typed::<FixedLenByteArrayType>()
            .write_batch(values, None, None)
            .map_err(parquet_error)?;
        column.close().map_err(parquet_error)?;
    }
    group.close().map_err(parquet_error)?;
    batch.clear();
    Ok(())
}

fn write_factors(
    path: &Path,
    c: &Connection,
    plan: &SnapshotPlan,
    compression: Compression,
    row_group_rows: usize,
) -> Result<FileStat, Error> {
    let mut writer = start(path, FACTOR_SCHEMA, compression, row_group_rows)?;
    let mut batch = FactorBatch::default();
    let mut rows = 0i64;
    for entry in plan
        .derived
        .iter()
        .filter(|entry| entry.dataset == super::DerivedDataset::AdjustmentEvents)
    {
        let count =
            read::for_each_factor(c, &entry.key, plan.request.start, plan.request.end, |row| {
                batch.push(row)?;
                if batch.rows >= row_group_rows {
                    flush_factors(&mut writer, &mut batch)?;
                }
                Ok(())
            })?;
        if count > entry.record_count
            || (plan.request.start.is_none()
                && plan.request.end.is_none()
                && count != entry.record_count)
        {
            return Err(Error::Integrity(format!(
                "factor set {} claims {} events but the table holds {count}",
                entry.key, entry.record_count
            )));
        }
        rows += count;
    }
    if batch.rows > 0 {
        flush_factors(&mut writer, &mut batch)?;
    }
    writer.close().map_err(parquet_error)?;
    file_stat(path, FACTORS_FILE, rows)
}

// ---------------------------------------------------------------------------
// calendar.parquet
// ---------------------------------------------------------------------------

fn write_calendar(
    path: &Path,
    c: &Connection,
    plan: &SnapshotPlan,
    compression: Compression,
    row_group_rows: usize,
) -> Result<FileStat, Error> {
    let mut writer = start(path, CALENDAR_SCHEMA, compression, row_group_rows)?;
    let mut days: Vec<i32> = Vec::new();
    let mut sources: Vec<i32> = Vec::new();
    let mut rows = 0i64;
    let flush = |writer: &mut SerializedFileWriter<File>,
                 days: &mut Vec<i32>,
                 sources: &mut Vec<i32>|
     -> Result<(), Error> {
        let mut group = writer.next_row_group().map_err(parquet_error)?;
        {
            let mut column = group
                .next_column()
                .map_err(parquet_error)?
                .ok_or_else(|| Error::Parquet("missing trade_day column".into()))?;
            column
                .typed::<Int32Type>()
                .write_batch(days, None, None)
                .map_err(parquet_error)?;
            column.close().map_err(parquet_error)?;
        }
        {
            let mut column = group
                .next_column()
                .map_err(parquet_error)?
                .ok_or_else(|| Error::Parquet("missing sources column".into()))?;
            column
                .typed::<Int32Type>()
                .write_batch(sources, None, None)
                .map_err(parquet_error)?;
            column.close().map_err(parquet_error)?;
        }
        group.close().map_err(parquet_error)?;
        days.clear();
        sources.clear();
        Ok(())
    };
    let count =
        read::for_each_calendar_day(c, plan.request.start, plan.request.end, |day, mask| {
            days.push(i32_value(compact_day(day))?);
            sources.push(i32_value(mask)?);
            if days.len() >= row_group_rows {
                flush(&mut writer, &mut days, &mut sources)?;
            }
            Ok(())
        })?;
    if !days.is_empty() {
        flush(&mut writer, &mut days, &mut sources)?;
    }
    writer.close().map_err(parquet_error)?;
    rows += count;
    file_stat(path, CALENDAR_FILE, rows)
}

// ---------------------------------------------------------------------------
// index_daily.parquet
// ---------------------------------------------------------------------------

#[derive(Default)]
struct IndexBatch {
    entity: Vec<ByteArray>,
    aliases: Vec<ByteArray>,
    day: Vec<i32>,
    open: Vec<i64>,
    high: Vec<i64>,
    low: Vec<i64>,
    close: Vec<i64>,
    price_scale: Vec<i32>,
    volume: Vec<i64>,
    volume_def: Vec<i16>,
    amount: Vec<FixedLenByteArray>,
    amount_def: Vec<i16>,
    aggregate_scope: Vec<ByteArray>,
    member_count: Vec<i32>,
    member_count_def: Vec<i16>,
    rows: usize,
}

impl IndexBatch {
    fn push(&mut self, entry: &SeriesEntry, row: BarRow) -> Result<(), Error> {
        let index = entry
            .index
            .as_ref()
            .ok_or_else(|| Error::Integrity("index series without metadata".into()))?;
        self.entity
            .push(ByteArray::from(entry.entity.as_bytes().to_vec()));
        self.aliases
            .push(ByteArray::from(index.aliases.as_bytes().to_vec()));
        self.day.push(i32_value(compact_day(row.trade_day))?);
        let scale = entry.price_scale;
        self.open.push(read::price_units(row.open_units, scale)?);
        self.high.push(read::price_units(row.high_units, scale)?);
        self.low.push(read::price_units(row.low_units, scale)?);
        self.close.push(read::price_units(row.close_units, scale)?);
        self.price_scale.push(i32_value(i64::from(scale))?);
        match row.volume {
            Some(volume) => {
                self.volume.push(volume);
                self.volume_def.push(1);
            }
            None => self.volume_def.push(0),
        }
        match row.amount {
            Some(amount) => {
                self.amount.push(fixed9(read::scaled_units(amount, 2)?)?);
                self.amount_def.push(1);
            }
            None => self.amount_def.push(0),
        }
        self.aggregate_scope
            .push(ByteArray::from(index.aggregate_scope.as_bytes().to_vec()));
        match index.member_count {
            Some(value) => {
                self.member_count.push(i32_value(value)?);
                self.member_count_def.push(1);
            }
            None => self.member_count_def.push(0),
        }
        self.rows += 1;
        Ok(())
    }

    fn clear(&mut self) {
        self.entity.clear();
        self.aliases.clear();
        self.day.clear();
        self.open.clear();
        self.high.clear();
        self.low.clear();
        self.close.clear();
        self.price_scale.clear();
        self.volume.clear();
        self.volume_def.clear();
        self.amount.clear();
        self.amount_def.clear();
        self.aggregate_scope.clear();
        self.member_count.clear();
        self.member_count_def.clear();
        self.rows = 0;
    }
}

fn flush_index(
    writer: &mut SerializedFileWriter<File>,
    batch: &mut IndexBatch,
) -> Result<(), Error> {
    let mut group = writer.next_row_group().map_err(parquet_error)?;
    for values in [&batch.entity, &batch.aliases] {
        let mut column = group
            .next_column()
            .map_err(parquet_error)?
            .ok_or_else(|| Error::Parquet("missing index string column".into()))?;
        column
            .typed::<ByteArrayType>()
            .write_batch(values, None, None)
            .map_err(parquet_error)?;
        column.close().map_err(parquet_error)?;
    }
    {
        let mut column = group
            .next_column()
            .map_err(parquet_error)?
            .ok_or_else(|| Error::Parquet("missing trade_day column".into()))?;
        column
            .typed::<Int32Type>()
            .write_batch(&batch.day, None, None)
            .map_err(parquet_error)?;
        column.close().map_err(parquet_error)?;
    }
    for values in [&batch.open, &batch.high, &batch.low, &batch.close] {
        let mut column = group
            .next_column()
            .map_err(parquet_error)?
            .ok_or_else(|| Error::Parquet("missing price column".into()))?;
        column
            .typed::<Int64Type>()
            .write_batch(values, None, None)
            .map_err(parquet_error)?;
        column.close().map_err(parquet_error)?;
    }
    {
        let mut column = group
            .next_column()
            .map_err(parquet_error)?
            .ok_or_else(|| Error::Parquet("missing price_scale column".into()))?;
        column
            .typed::<Int32Type>()
            .write_batch(&batch.price_scale, None, None)
            .map_err(parquet_error)?;
        column.close().map_err(parquet_error)?;
    }
    {
        let mut column = group
            .next_column()
            .map_err(parquet_error)?
            .ok_or_else(|| Error::Parquet("missing volume column".into()))?;
        column
            .typed::<Int64Type>()
            .write_batch(&batch.volume, Some(&batch.volume_def), None)
            .map_err(parquet_error)?;
        column.close().map_err(parquet_error)?;
    }
    {
        let mut column = group
            .next_column()
            .map_err(parquet_error)?
            .ok_or_else(|| Error::Parquet("missing amount column".into()))?;
        column
            .typed::<FixedLenByteArrayType>()
            .write_batch(&batch.amount, Some(&batch.amount_def), None)
            .map_err(parquet_error)?;
        column.close().map_err(parquet_error)?;
    }
    {
        let mut column = group
            .next_column()
            .map_err(parquet_error)?
            .ok_or_else(|| Error::Parquet("missing aggregate_scope column".into()))?;
        column
            .typed::<ByteArrayType>()
            .write_batch(&batch.aggregate_scope, None, None)
            .map_err(parquet_error)?;
        column.close().map_err(parquet_error)?;
    }
    {
        let mut column = group
            .next_column()
            .map_err(parquet_error)?
            .ok_or_else(|| Error::Parquet("missing member_count column".into()))?;
        column
            .typed::<Int32Type>()
            .write_batch(&batch.member_count, Some(&batch.member_count_def), None)
            .map_err(parquet_error)?;
        column.close().map_err(parquet_error)?;
    }
    group.close().map_err(parquet_error)?;
    batch.clear();
    Ok(())
}

fn write_index(
    path: &Path,
    c: &Connection,
    plan: &SnapshotPlan,
    compression: Compression,
    row_group_rows: usize,
) -> Result<FileStat, Error> {
    let mut writer = start(path, INDEX_SCHEMA, compression, row_group_rows)?;
    let mut batch = IndexBatch::default();
    let mut rows = 0i64;
    for entry in plan
        .series
        .iter()
        .filter(|entry| entry.instrument_class == crate::market_data::INSTRUMENT_CLASS_INDEX)
    {
        let count = read::for_each_bar(
            c,
            entry.series_key,
            plan.request.start,
            plan.request.end,
            |row| {
                batch.push(entry, row)?;
                if batch.rows >= row_group_rows {
                    flush_index(&mut writer, &mut batch)?;
                }
                Ok(())
            },
        )?;
        check_count(entry, count, plan)?;
        rows += count;
    }
    if batch.rows > 0 {
        flush_index(&mut writer, &mut batch)?;
    }
    writer.close().map_err(parquet_error)?;
    file_stat(path, INDEX_FILE, rows)
}

// ---------------------------------------------------------------------------
// M2 security-master and M3 whole-market files (§12.4, #446 §10.4)
// ---------------------------------------------------------------------------
//
// These nine tables are small (5k–200k rows) and already materialised by the
// read model, so the writer is one generic encoder instead of nine batching
// types: a row is a `Vec<Option<Cell>>` and the column's [`ColumnType`] says how
// to write it. The schema string, the manifest's column list and the type list
// are three views of the same contract; `m2_schemas_match_their_columns_and_types`
// parses every schema and compares all three (AC-M2-11 "新文件列类型", AC-M3-9),
// so a column cannot be added to one view and forgotten in another.

const SECURITY_MASTER_SCHEMA: &str = "
message security_master {
  REQUIRED BINARY exchange (STRING);
  REQUIRED BINARY code (STRING);
  REQUIRED INT32 effective_from (DATE);
  OPTIONAL INT32 effective_to (DATE);
  REQUIRED INT32 instrument_class (INTEGER(8,true));
  OPTIONAL BINARY issuer_key (STRING);
  REQUIRED BINARY issuer_resolution (STRING);
  OPTIONAL INT32 listing_date (DATE);
  REQUIRED BINARY listing_source (STRING);
  OPTIONAL INT32 delisting_date (DATE);
  REQUIRED BINARY delisting_source (STRING);
  OPTIONAL INT32 arrangement_start (DATE);
  OPTIONAL INT32 arrangement_end (DATE);
  OPTIONAL INT32 known_at (DATE);
  REQUIRED BINARY flags (STRING);
}";

const SECURITY_NAMES_SCHEMA: &str = "
message security_names {
  REQUIRED BINARY exchange (STRING);
  REQUIRED BINARY code (STRING);
  REQUIRED INT32 effective_from (DATE);
  OPTIONAL INT32 effective_to (DATE);
  REQUIRED BINARY name (STRING);
  REQUIRED BINARY certainty (STRING);
  OPTIONAL INT32 known_at (DATE);
  REQUIRED BINARY source_kind (STRING);
}";

const CAPITAL_EVENTS_SCHEMA: &str = "
message capital_events {
  REQUIRED BINARY exchange (STRING);
  REQUIRED BINARY code (STRING);
  REQUIRED INT64 ordinal;
  REQUIRED INT32 effective_day (DATE);
  REQUIRED INT32 category (INTEGER(8,true));
  REQUIRED INT64 float_after;
  REQUIRED INT64 total_after;
  OPTIONAL INT32 known_at (DATE);
  REQUIRED BOOLEAN suspicious;
}";

const RISK_WARNINGS_SCHEMA: &str = "
message risk_warnings {
  REQUIRED BINARY exchange (STRING);
  REQUIRED BINARY code (STRING);
  REQUIRED INT32 effective_from (DATE);
  OPTIONAL INT32 effective_to (DATE);
  REQUIRED BINARY normalized_status (STRING);
  REQUIRED BINARY raw_status_code (STRING);
  OPTIONAL BINARY announced_at (STRING);
  OPTIONAL INT32 known_at (DATE);
  REQUIRED BINARY source_kind (STRING);
}";

const SUSPENSIONS_SCHEMA: &str = "
message suspensions {
  REQUIRED BINARY exchange (STRING);
  REQUIRED BINARY code (STRING);
  REQUIRED INT32 start_day (DATE);
  REQUIRED INT32 end_day (DATE);
  REQUIRED BINARY certainty (STRING);
  REQUIRED INT32 missing_days;
  REQUIRED BINARY reason (STRING);
  OPTIONAL INT32 known_at (DATE);
}";

const PRICE_LIMIT_RULES_SCHEMA: &str = "
message price_limit_rules {
  REQUIRED BINARY rule_id (STRING);
  REQUIRED BINARY market (STRING);
  REQUIRED BINARY board (STRING);
  REQUIRED BINARY status_class (STRING);
  REQUIRED BINARY listing_phase (STRING);
  OPTIONAL INT64 band_bp;
  OPTIONAL INT64 min_band_ticks;
  REQUIRED BINARY band_side (STRING);
  REQUIRED BINARY rounding (STRING);
  REQUIRED INT32 effective_from (DATE);
  OPTIONAL INT32 effective_to (DATE);
  REQUIRED BINARY basis_ref (STRING);
  REQUIRED BINARY basis_status (STRING);
}";

const UNIVERSE_INTERVALS_SCHEMA: &str = "
message universe_intervals {
  REQUIRED BINARY entity (STRING);
  REQUIRED BINARY canonical_entity (STRING);
  REQUIRED BINARY aliases (STRING);
  REQUIRED INT32 member_from (DATE);
  OPTIONAL INT32 member_to (DATE);
  REQUIRED BINARY basis (STRING);
  REQUIRED BINARY flags (STRING);
}";

const INDUSTRY_ASSIGNMENTS_SCHEMA: &str = "
message industry_assignments {
  REQUIRED BINARY entity (STRING);
  REQUIRED BINARY exchange (STRING);
  REQUIRED BINARY code (STRING);
  REQUIRED BINARY scheme (STRING);
  REQUIRED INT64 level;
  REQUIRED BINARY industry_code (STRING);
  OPTIONAL BINARY name (STRING);
  REQUIRED BINARY name_status (STRING);
  REQUIRED INT32 effective_from (DATE);
  OPTIONAL INT32 effective_to (DATE);
  REQUIRED INT32 known_at (DATE);
  REQUIRED BINARY effective_from_basis (STRING);
  REQUIRED BINARY evidence_grade (STRING);
  REQUIRED BINARY source_snapshot_sha256 (STRING);
}";

const CODE_MIGRATIONS_SCHEMA: &str = "
message code_migrations {
  REQUIRED BINARY from (STRING);
  REQUIRED BINARY to (STRING);
  REQUIRED INT32 effective_from (DATE);
  REQUIRED BINARY certainty (STRING);
  REQUIRED BINARY source_kind (STRING);
  OPTIONAL INT32 known_at (DATE);
}";

/// The physical type of one exported column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ColumnType {
    Text,
    TextOpt,
    Date,
    DateOpt,
    Int8,
    Int32,
    Int64,
    Int64Opt,
    Bool,
}

impl ColumnType {
    fn is_optional(self) -> bool {
        matches!(self, Self::TextOpt | Self::DateOpt | Self::Int64Opt)
    }
}

/// One column's cells for one file. The encoder rejects a cell whose variant
/// does not match [`ColumnType`], so a row built with the wrong field fails
/// closed instead of writing a plausible-looking wrong file.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Cell {
    Text(String),
    Date(chrono::NaiveDate),
    Int8(i64),
    Int64(i64),
    Bool(bool),
}

type Row = Vec<Option<Cell>>;

struct TableSpec {
    path: &'static str,
    columns: &'static [&'static str],
    types: &'static [ColumnType],
    schema: &'static str,
}

const SECURITY_MASTER_SPEC: TableSpec = TableSpec {
    path: SECURITY_MASTER_FILE,
    columns: SECURITY_MASTER_COLUMNS,
    types: &[
        ColumnType::Text,
        ColumnType::Text,
        ColumnType::Date,
        ColumnType::DateOpt,
        ColumnType::Int8,
        ColumnType::TextOpt,
        ColumnType::Text,
        ColumnType::DateOpt,
        ColumnType::Text,
        ColumnType::DateOpt,
        ColumnType::Text,
        ColumnType::DateOpt,
        ColumnType::DateOpt,
        ColumnType::DateOpt,
        ColumnType::Text,
    ],
    schema: SECURITY_MASTER_SCHEMA,
};

const SECURITY_NAMES_SPEC: TableSpec = TableSpec {
    path: SECURITY_NAMES_FILE,
    columns: SECURITY_NAMES_COLUMNS,
    types: &[
        ColumnType::Text,
        ColumnType::Text,
        ColumnType::Date,
        ColumnType::DateOpt,
        ColumnType::Text,
        ColumnType::Text,
        ColumnType::DateOpt,
        ColumnType::Text,
    ],
    schema: SECURITY_NAMES_SCHEMA,
};

const CAPITAL_EVENTS_SPEC: TableSpec = TableSpec {
    path: CAPITAL_EVENTS_FILE,
    columns: CAPITAL_EVENTS_COLUMNS,
    types: &[
        ColumnType::Text,
        ColumnType::Text,
        ColumnType::Int64,
        ColumnType::Date,
        ColumnType::Int8,
        ColumnType::Int64,
        ColumnType::Int64,
        ColumnType::DateOpt,
        ColumnType::Bool,
    ],
    schema: CAPITAL_EVENTS_SCHEMA,
};

const RISK_WARNINGS_SPEC: TableSpec = TableSpec {
    path: RISK_WARNINGS_FILE,
    columns: RISK_WARNINGS_COLUMNS,
    types: &[
        ColumnType::Text,
        ColumnType::Text,
        ColumnType::Date,
        ColumnType::DateOpt,
        ColumnType::Text,
        ColumnType::Text,
        ColumnType::TextOpt,
        ColumnType::DateOpt,
        ColumnType::Text,
    ],
    schema: RISK_WARNINGS_SCHEMA,
};

const SUSPENSIONS_SPEC: TableSpec = TableSpec {
    path: SUSPENSIONS_FILE,
    columns: SUSPENSIONS_COLUMNS,
    types: &[
        ColumnType::Text,
        ColumnType::Text,
        ColumnType::Date,
        ColumnType::Date,
        ColumnType::Text,
        ColumnType::Int32,
        ColumnType::Text,
        ColumnType::DateOpt,
    ],
    schema: SUSPENSIONS_SCHEMA,
};

const PRICE_LIMIT_RULES_SPEC: TableSpec = TableSpec {
    path: PRICE_LIMIT_RULES_FILE,
    columns: PRICE_LIMIT_RULES_COLUMNS,
    types: &[
        ColumnType::Text,
        ColumnType::Text,
        ColumnType::Text,
        ColumnType::Text,
        ColumnType::Text,
        ColumnType::Int64Opt,
        ColumnType::Int64Opt,
        ColumnType::Text,
        ColumnType::Text,
        ColumnType::Date,
        ColumnType::DateOpt,
        ColumnType::Text,
        ColumnType::Text,
    ],
    schema: PRICE_LIMIT_RULES_SCHEMA,
};

const UNIVERSE_INTERVALS_SPEC: TableSpec = TableSpec {
    path: UNIVERSE_INTERVALS_FILE,
    columns: UNIVERSE_INTERVALS_COLUMNS,
    types: &[
        ColumnType::Text,
        ColumnType::Text,
        ColumnType::Text,
        ColumnType::Date,
        ColumnType::DateOpt,
        ColumnType::Text,
        ColumnType::Text,
    ],
    schema: UNIVERSE_INTERVALS_SCHEMA,
};

const INDUSTRY_ASSIGNMENTS_SPEC: TableSpec = TableSpec {
    path: INDUSTRY_ASSIGNMENTS_FILE,
    columns: INDUSTRY_ASSIGNMENTS_COLUMNS,
    types: &[
        ColumnType::Text,
        ColumnType::Text,
        ColumnType::Text,
        ColumnType::Text,
        ColumnType::Int64,
        ColumnType::Text,
        ColumnType::TextOpt,
        ColumnType::Text,
        ColumnType::Date,
        ColumnType::DateOpt,
        ColumnType::Date,
        ColumnType::Text,
        ColumnType::Text,
        ColumnType::Text,
    ],
    schema: INDUSTRY_ASSIGNMENTS_SCHEMA,
};

const CODE_MIGRATIONS_SPEC: TableSpec = TableSpec {
    path: CODE_MIGRATIONS_FILE,
    columns: CODE_MIGRATIONS_COLUMNS,
    types: &[
        ColumnType::Text,
        ColumnType::Text,
        ColumnType::Date,
        ColumnType::Text,
        ColumnType::Text,
        ColumnType::DateOpt,
    ],
    schema: CODE_MIGRATIONS_SCHEMA,
};

fn text(value: impl Into<String>) -> Option<Cell> {
    Some(Cell::Text(value.into()))
}

fn opt_text(value: Option<impl Into<String>>) -> Option<Cell> {
    value.map(|value| Cell::Text(value.into()))
}

fn date(value: chrono::NaiveDate) -> Option<Cell> {
    Some(Cell::Date(value))
}

fn opt_date(value: Option<chrono::NaiveDate>) -> Option<Cell> {
    value.map(Cell::Date)
}

fn join_flags(flags: &[&str]) -> Option<Cell> {
    text(flags.join(","))
}

fn write_column(
    column: &mut parquet::file::writer::SerializedColumnWriter<'_>,
    kind: ColumnType,
    cells: &[Option<Cell>],
    name: &str,
) -> Result<(), Error> {
    let mut texts: Vec<ByteArray> = Vec::new();
    let mut ints: Vec<i32> = Vec::new();
    let mut longs: Vec<i64> = Vec::new();
    let mut bools: Vec<bool> = Vec::new();
    let mut defs: Vec<i16> = Vec::with_capacity(cells.len());
    let mismatch = || Error::Integrity(format!("{name}: cell type does not match the schema"));
    for cell in cells {
        match (kind, cell) {
            (ColumnType::Text | ColumnType::TextOpt, Some(Cell::Text(value))) => {
                texts.push(ByteArray::from(value.as_bytes().to_vec()));
                defs.push(1);
            }
            (ColumnType::Text | ColumnType::TextOpt, None) => defs.push(0),
            (ColumnType::Date, Some(Cell::Date(value))) => {
                ints.push(i32_value(compact_day(*value))?);
                defs.push(1);
            }
            (ColumnType::Date, None) => return Err(mismatch()),
            (ColumnType::DateOpt, Some(Cell::Date(value))) => {
                ints.push(i32_value(compact_day(*value))?);
                defs.push(1);
            }
            (ColumnType::DateOpt, None) => defs.push(0),
            (ColumnType::Int8, Some(Cell::Int8(value))) => {
                ints.push(i32_value(*value)?);
                defs.push(0);
            }
            (ColumnType::Int32, Some(Cell::Int64(value))) => {
                ints.push(i32_value(*value)?);
                defs.push(0);
            }
            (ColumnType::Int8 | ColumnType::Int32, None) => return Err(mismatch()),
            (ColumnType::Int64, Some(Cell::Int64(value))) => {
                longs.push(*value);
                defs.push(0);
            }
            (ColumnType::Int64Opt, Some(Cell::Int64(value))) => {
                longs.push(*value);
                defs.push(1);
            }
            (ColumnType::Int64Opt, None) => defs.push(0),
            (ColumnType::Int64, None) => return Err(mismatch()),
            (ColumnType::Bool, Some(Cell::Bool(value))) => {
                bools.push(*value);
                defs.push(0);
            }
            (ColumnType::Bool, None) => return Err(mismatch()),
            (_, Some(_)) => return Err(mismatch()),
        }
    }
    let levels = kind.is_optional().then_some(defs.as_slice());
    // The writers return the number of values written; the caller already
    // knows it (`cells.len()`), so the count is discarded here.
    let _ = match kind {
        ColumnType::Text | ColumnType::TextOpt => column
            .typed::<ByteArrayType>()
            .write_batch(&texts, levels, None)
            .map_err(parquet_error)?,
        ColumnType::Date | ColumnType::DateOpt | ColumnType::Int8 | ColumnType::Int32 => column
            .typed::<Int32Type>()
            .write_batch(&ints, levels, None)
            .map_err(parquet_error)?,
        ColumnType::Int64 | ColumnType::Int64Opt => column
            .typed::<Int64Type>()
            .write_batch(&longs, levels, None)
            .map_err(parquet_error)?,
        ColumnType::Bool => column
            .typed::<BoolType>()
            .write_batch(&bools, levels, None)
            .map_err(parquet_error)?,
    };
    Ok(())
}

/// Encode one table (§12.4). `rows` are already ordered by the caller.
fn write_table(
    path: &Path,
    spec: &TableSpec,
    rows: &[Row],
    compression: Compression,
    row_group_rows: usize,
) -> Result<FileStat, Error> {
    if rows.iter().any(|row| row.len() != spec.columns.len()) {
        return Err(Error::Integrity(format!(
            "{}: a row does not have {} columns",
            spec.path,
            spec.columns.len()
        )));
    }
    let mut writer = start(path, spec.schema, compression, row_group_rows)?;
    for chunk in rows.chunks(row_group_rows.max(1)) {
        let mut group = writer.next_row_group().map_err(parquet_error)?;
        for (index, name) in spec.columns.iter().enumerate() {
            let mut column = group
                .next_column()
                .map_err(parquet_error)?
                .ok_or_else(|| Error::Parquet(format!("{}: missing {name}", spec.path)))?;
            let cells: Vec<Option<Cell>> = chunk.iter().map(|row| row[index].clone()).collect();
            write_column(&mut column, spec.types[index], &cells, name)?;
            // `close` consumes the writer, so it happens here where the
            // column is owned, not in `write_column`.
            column.close().map_err(parquet_error)?;
        }
        group.close().map_err(parquet_error)?;
    }
    writer.close().map_err(parquet_error)?;
    file_stat(path, spec.path, rows.len() as i64)
}

fn security_master_rows(c: &Connection, plan: &SnapshotPlan) -> Result<Vec<Row>, Error> {
    let rows = read::security_master_rows(c, &plan.request)?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let mut flags: Vec<&str> = Vec::new();
            if row.listing_conflict {
                flags.push("listing_conflict");
            }
            if row.delisting_conflict {
                flags.push("delisting_conflict");
            }
            if row.listing_date.is_none() {
                flags.push("listing_unknown");
            }
            if row.delisting_date.is_none() {
                flags.push("delisting_unknown");
            }
            if row.issuer_key.is_none() {
                flags.push("issuer_key_unknown");
            }
            vec![
                text(row.exchange.as_str()),
                text(row.code),
                date(row.effective_from),
                opt_date(row.effective_to),
                Some(Cell::Int8(i64::from(row.instrument_class))),
                opt_text(row.issuer_key),
                text(row.issuer_resolution.as_str()),
                opt_date(row.listing_date),
                text(row.listing_source.as_str()),
                opt_date(row.delisting_date),
                text(row.delisting_source.as_str()),
                opt_date(row.arrangement_start),
                opt_date(row.arrangement_end),
                opt_date(row.known_at),
                join_flags(&flags),
            ]
        })
        .collect())
}

fn security_name_rows(c: &Connection, plan: &SnapshotPlan) -> Result<Vec<Row>, Error> {
    let rows = read::security_name_rows(c, &plan.request)?;
    Ok(rows
        .into_iter()
        .map(|row| {
            vec![
                text(row.exchange),
                text(row.code),
                date(row.effective_from),
                opt_date(row.effective_to),
                text(row.name),
                text(row.certainty),
                opt_date(row.known_at),
                text(row.source_kind),
            ]
        })
        .collect())
}

fn capital_event_rows(c: &Connection, plan: &SnapshotPlan) -> Result<Vec<Row>, Error> {
    let rows = read::capital_event_rows(c, &plan.request)?;
    Ok(rows
        .into_iter()
        .map(|(entity, event)| {
            let (exchange, code) = entity.split_once('.').unwrap_or((entity.as_str(), ""));
            vec![
                text(exchange),
                text(code),
                Some(Cell::Int64(event.ordinal)),
                date(event.effective_day),
                Some(Cell::Int8(i64::from(event.category))),
                Some(Cell::Int64(event.float_after)),
                Some(Cell::Int64(event.total_after)),
                opt_date(event.known_at),
                Some(Cell::Bool(event.suspicious)),
            ]
        })
        .collect())
}

fn risk_warning_rows(c: &Connection, plan: &SnapshotPlan) -> Result<Vec<Row>, Error> {
    let rows = read::risk_warning_rows(c, &plan.request)?;
    Ok(rows
        .into_iter()
        .map(|row| {
            vec![
                text(row.exchange.as_str()),
                text(row.code),
                date(row.effective_from),
                opt_date(row.effective_to),
                text(row.status.as_str()),
                text(row.raw_status_code),
                opt_text(row.announced_at),
                opt_date(row.known_at),
                text(row.source_kind),
            ]
        })
        .collect())
}

fn suspension_rows(c: &Connection, plan: &SnapshotPlan) -> Result<Vec<Row>, Error> {
    let rows = read::suspension_rows(c, &plan.request)?;
    Ok(rows
        .into_iter()
        .map(|row| {
            vec![
                text(row.exchange.as_str()),
                text(row.code),
                date(row.start_day),
                date(row.end_day),
                text(row.certainty.as_str()),
                Some(Cell::Int64(row.missing_days)),
                text(row.reason.as_str()),
                opt_date(row.known_at),
            ]
        })
        .collect())
}

fn price_limit_rule_rows(c: &Connection, plan: &SnapshotPlan) -> Result<Vec<Row>, Error> {
    let rows = read::price_limit_rule_rows(c, &plan.request)?;
    Ok(rows
        .into_iter()
        .map(|row| {
            vec![
                text(row.rule_id),
                text(row.market.as_str()),
                text(row.board.as_str()),
                text(row.status_class.as_str()),
                text(row.listing_phase.as_str()),
                row.band_bp.map(Cell::Int64),
                row.min_band_ticks.map(Cell::Int64),
                text(row.band_side.as_str()),
                text(row.rounding.as_str()),
                date(row.effective_from),
                opt_date(row.effective_to),
                text(row.basis_ref),
                text(row.basis_status.as_str()),
            ]
        })
        .collect())
}

fn universe_interval_rows(c: &Connection, plan: &SnapshotPlan) -> Result<Vec<Row>, Error> {
    let rows = read::universe_interval_rows(c, &plan.request)?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let flags: Vec<&str> = row.flags.iter().map(|flag| flag.as_str()).collect();
            vec![
                text(row.entity),
                text(row.canonical_entity),
                text(row.aliases.join(",")),
                date(row.member_from),
                opt_date(row.member_to),
                text(row.basis.as_str()),
                join_flags(&flags),
            ]
        })
        .collect())
}

fn industry_assignment_rows(c: &Connection, plan: &SnapshotPlan) -> Result<Vec<Row>, Error> {
    let rows = read::industry_assignment_rows(c, &plan.request)?;
    Ok(rows
        .into_iter()
        .map(|row| {
            vec![
                text(row.entity),
                text(row.exchange),
                text(row.code),
                text(row.scheme),
                Some(Cell::Int64(row.level)),
                text(row.industry_code),
                opt_text(row.name),
                text(row.name_status),
                date(row.effective_from),
                opt_date(row.effective_to),
                date(row.known_at),
                text(row.effective_from_basis),
                text(row.evidence_grade),
                text(row.source_snapshot_sha256),
            ]
        })
        .collect())
}

fn code_migration_rows(c: &Connection, plan: &SnapshotPlan) -> Result<Vec<Row>, Error> {
    let rows = read::code_migration_rows(c, &plan.request)?;
    Ok(rows
        .into_iter()
        .map(|row| {
            vec![
                text(row.from_entity()),
                text(row.to_entity()),
                date(row.effective_from),
                text(row.certainty.as_str()),
                text(row.source_kind),
                opt_date(row.known_at),
            ]
        })
        .collect())
}

/// The table spec of one whole-market file, or `None` for a file this module
/// does not write with the generic encoder. Shared by [`write_all`] and the
/// schema test, so "planned but not writable" cannot hide.
fn table_spec(path: &str) -> Option<&'static TableSpec> {
    Some(match path {
        SECURITY_MASTER_FILE => &SECURITY_MASTER_SPEC,
        SECURITY_NAMES_FILE => &SECURITY_NAMES_SPEC,
        CAPITAL_EVENTS_FILE => &CAPITAL_EVENTS_SPEC,
        RISK_WARNINGS_FILE => &RISK_WARNINGS_SPEC,
        SUSPENSIONS_FILE => &SUSPENSIONS_SPEC,
        PRICE_LIMIT_RULES_FILE => &PRICE_LIMIT_RULES_SPEC,
        UNIVERSE_INTERVALS_FILE => &UNIVERSE_INTERVALS_SPEC,
        INDUSTRY_ASSIGNMENTS_FILE => &INDUSTRY_ASSIGNMENTS_SPEC,
        CODE_MIGRATIONS_FILE => &CODE_MIGRATIONS_SPEC,
        _ => return None,
    })
}

fn table_rows(path: &str, c: &Connection, plan: &SnapshotPlan) -> Result<Vec<Row>, Error> {
    match path {
        SECURITY_MASTER_FILE => security_master_rows(c, plan),
        SECURITY_NAMES_FILE => security_name_rows(c, plan),
        CAPITAL_EVENTS_FILE => capital_event_rows(c, plan),
        RISK_WARNINGS_FILE => risk_warning_rows(c, plan),
        SUSPENSIONS_FILE => suspension_rows(c, plan),
        PRICE_LIMIT_RULES_FILE => price_limit_rule_rows(c, plan),
        UNIVERSE_INTERVALS_FILE => universe_interval_rows(c, plan),
        INDUSTRY_ASSIGNMENTS_FILE => industry_assignment_rows(c, plan),
        CODE_MIGRATIONS_FILE => code_migration_rows(c, plan),
        other => Err(Error::Integrity(format!("no row source for {other}"))),
    }
}

/// Write every file the plan names, in plan order (§7.2).
pub(crate) fn write_all(
    directory: &Path,
    c: &Connection,
    plan: &SnapshotPlan,
    compression: Compression,
    row_group_rows: usize,
) -> Result<Vec<FileStat>, Error> {
    let mut files = Vec::new();
    for file in &plan.files {
        let path = directory.join(file.path);
        let stat = match file.path {
            DAILY_RAW_FILE => write_daily(
                &path,
                DAILY_RAW_FILE,
                c,
                plan,
                Adjustment::Raw,
                compression,
                row_group_rows,
            )?,
            DAILY_QFQ_FILE => write_daily(
                &path,
                DAILY_QFQ_FILE,
                c,
                plan,
                Adjustment::Forward,
                compression,
                row_group_rows,
            )?,
            FACTORS_FILE => write_factors(&path, c, plan, compression, row_group_rows)?,
            CALENDAR_FILE => write_calendar(&path, c, plan, compression, row_group_rows)?,
            INDEX_FILE => write_index(&path, c, plan, compression, row_group_rows)?,
            other => match table_spec(other) {
                Some(spec) => write_table(
                    &path,
                    spec,
                    &table_rows(spec.path, c, plan)?,
                    compression,
                    row_group_rows,
                )?,
                None => {
                    return Err(Error::Integrity(format!(
                        "no writer for planned file {other}"
                    )));
                }
            },
        };
        files.push(stat);
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use parquet::basic::LogicalType;
    use parquet::basic::Type as PhysicalType;

    fn specs() -> Vec<&'static TableSpec> {
        vec![
            &SECURITY_MASTER_SPEC,
            &SECURITY_NAMES_SPEC,
            &CAPITAL_EVENTS_SPEC,
            &RISK_WARNINGS_SPEC,
            &SUSPENSIONS_SPEC,
            &PRICE_LIMIT_RULES_SPEC,
            &UNIVERSE_INTERVALS_SPEC,
            &INDUSTRY_ASSIGNMENTS_SPEC,
            &CODE_MIGRATIONS_SPEC,
        ]
    }

    fn physical(kind: ColumnType) -> PhysicalType {
        match kind {
            ColumnType::Text | ColumnType::TextOpt => PhysicalType::BYTE_ARRAY,
            ColumnType::Date | ColumnType::DateOpt | ColumnType::Int8 | ColumnType::Int32 => {
                PhysicalType::INT32
            }
            ColumnType::Int64 | ColumnType::Int64Opt => PhysicalType::INT64,
            ColumnType::Bool => PhysicalType::BOOLEAN,
        }
    }

    /// The logical type each column kind must declare. `INTEGER(8,true)` is
    /// the only width the M2 files use (`instrument_class`, `category`).
    fn assert_logical(kind: ColumnType, logical: Option<&LogicalType>, column: &str) {
        match kind {
            ColumnType::Text | ColumnType::TextOpt => assert!(
                matches!(logical, Some(LogicalType::String)),
                "{column}: expected STRING"
            ),
            ColumnType::Date | ColumnType::DateOpt => assert!(
                matches!(logical, Some(LogicalType::Date)),
                "{column}: expected DATE"
            ),
            ColumnType::Int8 => match logical {
                Some(LogicalType::Integer(int)) => {
                    assert_eq!((int.bit_width, int.is_signed), (8, true), "{column}");
                }
                other => panic!("{column}: expected INTEGER(8,true), got {other:?}"),
            },
            ColumnType::Int32 | ColumnType::Int64 | ColumnType::Int64Opt | ColumnType::Bool => {
                assert!(logical.is_none(), "{column}: expected no logical type")
            }
        }
    }

    /// AC-M2-11 "新文件列类型": the schema string, the column list and the type
    /// list are three views of one contract, and they agree. A column added to
    /// the schema only would fail here instead of reaching a consumer.
    #[test]
    fn m2_schemas_match_their_columns_and_types() {
        for spec in specs() {
            assert_eq!(
                spec.columns.len(),
                spec.types.len(),
                "{}: column/type length",
                spec.path
            );
            let schema = parse_message_type(spec.schema).unwrap();
            let fields = schema.get_fields();
            assert_eq!(fields.len(), spec.columns.len(), "{}", spec.path);
            for (index, field) in fields.iter().enumerate() {
                let kind = spec.types[index];
                assert_eq!(
                    field.get_basic_info().name(),
                    spec.columns[index],
                    "{}: column {index}",
                    spec.path
                );
                assert_eq!(
                    field.is_optional(),
                    kind.is_optional(),
                    "{}: {} optional",
                    spec.path,
                    spec.columns[index]
                );
                assert_eq!(
                    field.get_physical_type(),
                    physical(kind),
                    "{}: {} physical type",
                    spec.path,
                    spec.columns[index]
                );
                assert_logical(
                    kind,
                    field.get_basic_info().logical_type_ref(),
                    &format!("{}: {}", spec.path, spec.columns[index]),
                );
            }
        }
    }

    /// The manifest's column list is the schema's, in the same order, for every
    /// file the export plans (M1, M2 and M3). `daily_raw` is included so the M1
    /// files cannot drift either.
    #[test]
    fn every_planned_table_file_has_a_spec_with_the_manifest_columns() {
        let request = crate::research::ExportRequest::default();
        for file in crate::research::file_plans(&request) {
            let Some(spec) = table_spec(file.path) else {
                continue;
            };
            assert_eq!(file.columns, spec.columns, "{}", file.path);
            assert_eq!(spec.path, file.path);
        }
        // The security selection must plan exactly the M2 files plus the M3
        // whole-market additions (#446 §10.4), in that order.
        let planned: Vec<&str> = crate::research::file_plans(&crate::research::ExportRequest {
            dataset: crate::research::DatasetSelection::Security,
            ..Default::default()
        })
        .into_iter()
        .map(|file| file.path)
        .collect();
        assert_eq!(
            planned,
            vec![
                SECURITY_MASTER_FILE,
                SECURITY_NAMES_FILE,
                CAPITAL_EVENTS_FILE,
                RISK_WARNINGS_FILE,
                SUSPENSIONS_FILE,
                PRICE_LIMIT_RULES_FILE,
                UNIVERSE_INTERVALS_FILE,
                INDUSTRY_ASSIGNMENTS_FILE,
                CODE_MIGRATIONS_FILE,
            ]
        );
    }
}
