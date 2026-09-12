//! Structural .day decoding. Raw fields are not approved business prices/units.
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const RECORD_BYTES: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawDayRecord {
    pub date: NaiveDate,
    pub ohlc_raw: [u32; 4],
    pub amount_float32_bits: u32,
    pub volume_raw: u32,
    pub reserved: u32,
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
    for (index, chunk) in bytes.chunks_exact(RECORD_BYTES).enumerate() {
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
            reserved: words[7],
        });
    }
    Ok(records)
}

pub fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
