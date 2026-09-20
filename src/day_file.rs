//! Structural .day decoding. Raw fields are not approved business prices/units.
pub use crate::hash::sha256;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

pub const RECORD_BYTES: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawDayRecord {
    pub date: NaiveDate,
    pub ohlc_raw: [u32; 4],
    pub amount_float32_bits: u32,
    pub volume_raw: u32,
    /// Eighth .day word, preserved verbatim. A-share ex-rights basis price
    /// (fen) evidence is anchored only for 1999–2010 (#240); fund/index
    /// members carry different semantics, and post-2010 values degrade to
    /// the 0x10000 sentinel — consumers must not interpret it as a price.
    pub r7_raw: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DayFileError {
    #[error("day file byte count {0} is not divisible by 32")]
    Truncated(usize),
    #[error("invalid calendar date {value} at record {index}")]
    InvalidDate { index: usize, value: u32 },
    #[error("non-increasing date at record {0}")]
    DateOrder(usize),
}

pub fn parse(bytes: &[u8]) -> Result<Vec<RawDayRecord>, DayFileError> {
    if !bytes.len().is_multiple_of(RECORD_BYTES) {
        return Err(DayFileError::Truncated(bytes.len()));
    }
    let mut records: Vec<RawDayRecord> = Vec::with_capacity(bytes.len() / RECORD_BYTES);
    for (index, chunk) in bytes.as_chunks::<RECORD_BYTES>().0.iter().enumerate() {
        let words: [u32; 8] = std::array::from_fn(|i| {
            u32::from_le_bytes(chunk[i * 4..i * 4 + 4].try_into().unwrap())
        });
        let raw = words[0];
        let year = raw / 10000;
        let date = if (1..=9999).contains(&year) {
            NaiveDate::from_ymd_opt(year as i32, raw / 100 % 100, raw % 100)
        } else {
            None
        }
        .ok_or(DayFileError::InvalidDate { index, value: raw })?;
        if records.last().is_some_and(|previous| previous.date >= date) {
            return Err(DayFileError::DateOrder(index));
        }
        records.push(RawDayRecord {
            date,
            ohlc_raw: [words[1], words[2], words[3], words[4]],
            amount_float32_bits: words[5],
            volume_raw: words[6],
            r7_raw: words[7],
        });
    }
    Ok(records)
}
