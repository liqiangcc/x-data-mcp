//! Lossless structural handling for retained GBBQ evidence.
//!
//! The 29-byte record layout is observed in the retained community decoder
//! evidence, not an official TDX semantic contract.  This module therefore
//! exposes encrypted records only and cannot produce events, factors, prices,
//! or a publishable market batch.

use std::convert::TryInto;

pub const RECORD_BYTES: usize = 29;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawFile {
    pub record_count: u32,
    pub records: Vec<[u8; RECORD_BYTES]>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("GBBQ file is shorter than its count header: {0} bytes")]
    HeaderTruncated(usize),
    #[error("GBBQ length {actual} does not equal 4 + count({count}) * 29 = {expected}")]
    LengthMismatch {
        actual: usize,
        count: u32,
        expected: usize,
    },
    #[error("GBBQ record count does not fit host memory")]
    RecordCountOverflow,
}

/// Validate the observed container shape while preserving every encrypted
/// record. No field or category is interpreted here.
pub fn parse(bytes: &[u8]) -> Result<RawFile, Error> {
    if bytes.len() < 4 {
        return Err(Error::HeaderTruncated(bytes.len()));
    }
    let record_count = u32::from_le_bytes(bytes[..4].try_into().unwrap());
    let count = usize::try_from(record_count).map_err(|_| Error::RecordCountOverflow)?;
    let expected = count
        .checked_mul(RECORD_BYTES)
        .and_then(|size| size.checked_add(4))
        .ok_or(Error::RecordCountOverflow)?;
    if bytes.len() != expected {
        return Err(Error::LengthMismatch {
            actual: bytes.len(),
            count: record_count,
            expected,
        });
    }
    let records = bytes[4..].as_chunks::<RECORD_BYTES>().0.to_vec();
    Ok(RawFile {
        record_count,
        records,
    })
}
