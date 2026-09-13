//! Conservative dBase III/IV container parsing for retained source evidence.
//!
//! This module only validates the container and preserves field bytes.  It
//! deliberately does not decode the code page or assign business meaning to
//! any field; those decisions require an independent official contract.

use std::convert::TryInto;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDescriptor {
    pub name: String,
    pub field_type: u8,
    pub length: u8,
    pub decimals: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbfRecord {
    /// The deletion marker is retained instead of silently dropping rows.
    pub deleted: bool,
    /// Bytes after the deletion marker, exactly as stored in the file.
    pub raw: Vec<u8>,
}

impl DbfRecord {
    pub fn field_bytes(&self, fields: &[FieldDescriptor], index: usize) -> Option<&[u8]> {
        let field = fields.get(index)?;
        let offset = fields[..index]
            .iter()
            .map(|descriptor| usize::from(descriptor.length))
            .sum::<usize>();
        let end = offset.checked_add(usize::from(field.length))?;
        self.raw.get(offset..end)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbfFile {
    pub version: u8,
    pub last_update: [u8; 3],
    pub record_count: u32,
    pub header_length: u16,
    pub record_length: u16,
    pub code_page: u8,
    pub fields: Vec<FieldDescriptor>,
    pub records: Vec<DbfRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DbfError {
    #[error("DBF header is truncated: {0} bytes")]
    HeaderTruncated(usize),
    #[error("unsupported DBF version 0x{0:02x}")]
    UnsupportedVersion(u8),
    #[error("DBF header length {header} is smaller than field descriptors {minimum}")]
    InvalidHeaderLength { header: u16, minimum: u16 },
    #[error("DBF record length must include deletion marker")]
    InvalidRecordLength,
    #[error("DBF field descriptor area is truncated")]
    FieldDescriptorTruncated,
    #[error("DBF field {index} has zero length")]
    ZeroLengthField { index: usize },
    #[error("DBF fields require {required} bytes but record length is {record_length}")]
    FieldsExceedRecord {
        required: usize,
        record_length: usize,
    },
    #[error("DBF body is truncated: expected {expected} bytes, got {actual}")]
    BodyTruncated { expected: usize, actual: usize },
    #[error("DBF record count does not fit host memory")]
    RecordCountOverflow,
}

/// Parse a DBF container without interpreting its text or numeric fields.
pub fn parse(bytes: &[u8]) -> Result<DbfFile, DbfError> {
    if bytes.len() < 32 {
        return Err(DbfError::HeaderTruncated(bytes.len()));
    }
    let version = bytes[0];
    // Common dBase III/IV variants used by retained TDX evidence. Memo and
    // SQL-table variants are intentionally rejected until their semantics are
    // separately specified.
    if !matches!(version, 0x03 | 0x83 | 0x30 | 0x31 | 0x32 | 0x43 | 0x63) {
        return Err(DbfError::UnsupportedVersion(version));
    }
    let header_length = u16::from_le_bytes(bytes[8..10].try_into().unwrap());
    let record_length = u16::from_le_bytes(bytes[10..12].try_into().unwrap());
    let record_count = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    let minimum_header = 32u16.saturating_add(1);
    if header_length < minimum_header {
        return Err(DbfError::InvalidHeaderLength {
            header: header_length,
            minimum: minimum_header,
        });
    }
    if record_length < 1 {
        return Err(DbfError::InvalidRecordLength);
    }
    let header_end = usize::from(header_length);
    if bytes.len() < header_end {
        return Err(DbfError::HeaderTruncated(bytes.len()));
    }
    let descriptor_end = header_end - 1;
    if bytes[descriptor_end] != 0x0d {
        return Err(DbfError::FieldDescriptorTruncated);
    }

    let mut fields = Vec::new();
    let mut offset = 32usize;
    while offset < descriptor_end {
        let end = offset
            .checked_add(32)
            .ok_or(DbfError::FieldDescriptorTruncated)?;
        if end > descriptor_end {
            return Err(DbfError::FieldDescriptorTruncated);
        }
        let descriptor = &bytes[offset..end];
        let name_end = descriptor[..11]
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(11);
        let name = String::from_utf8_lossy(&descriptor[..name_end]).into_owned();
        let length = descriptor[16];
        if length == 0 {
            return Err(DbfError::ZeroLengthField {
                index: fields.len(),
            });
        }
        fields.push(FieldDescriptor {
            name,
            field_type: descriptor[11],
            length,
            decimals: descriptor[17],
        });
        offset = end;
    }
    let field_bytes = fields
        .iter()
        .map(|field| usize::from(field.length))
        .try_fold(0usize, usize::checked_add)
        .ok_or(DbfError::FieldsExceedRecord {
            required: usize::MAX,
            record_length: usize::from(record_length),
        })?;
    if field_bytes + 1 > usize::from(record_length) {
        return Err(DbfError::FieldsExceedRecord {
            required: field_bytes + 1,
            record_length: usize::from(record_length),
        });
    }
    let body_length = usize::try_from(record_count)
        .ok()
        .and_then(|count| count.checked_mul(usize::from(record_length)))
        .ok_or(DbfError::RecordCountOverflow)?;
    let body_start = header_end;
    let expected = body_start
        .checked_add(body_length)
        .ok_or(DbfError::RecordCountOverflow)?;
    if bytes.len() < expected {
        return Err(DbfError::BodyTruncated {
            expected,
            actual: bytes.len(),
        });
    }
    let mut records = Vec::with_capacity(usize::try_from(record_count).unwrap_or(0));
    for index in 0..usize::try_from(record_count).unwrap_or(0) {
        let start = body_start + index * usize::from(record_length);
        let record = &bytes[start..start + usize::from(record_length)];
        records.push(DbfRecord {
            deleted: record[0] == b'*',
            raw: record[1..1 + field_bytes].to_vec(),
        });
    }
    Ok(DbfFile {
        version,
        last_update: bytes[1..4].try_into().unwrap(),
        record_count,
        header_length,
        record_length,
        code_page: bytes[29],
        fields,
        records,
    })
}
