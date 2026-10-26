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
    ByteArray, ByteArrayType, FixedLenByteArray, FixedLenByteArrayType, Int32Type, Int64Type,
};
use parquet::file::properties::WriterProperties;
use parquet::file::writer::SerializedFileWriter;
use parquet::schema::parser::parse_message_type;
use rusqlite::Connection;

use super::export::Compression;
use super::read::{self, BarRow, FactorRow};
use super::{
    CALENDAR_FILE, DAILY_QFQ_FILE, DAILY_RAW_FILE, Error, FACTORS_FILE, FileStat, INDEX_FILE,
    SeriesEntry, SnapshotPlan,
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
        .set_max_row_group_row_count(row_group_rows)
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
        self.category.push(i32_value(i64::from(row.category))?);
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
            other => {
                return Err(Error::Integrity(format!(
                    "no writer for planned file {other}"
                )));
            }
        };
        files.push(stat);
    }
    Ok(files)
}
