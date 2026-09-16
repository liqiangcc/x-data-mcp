//! Bounded `infoharbor_ex.code` parser (design: docs/tasks/issue-6-catalog-impl.md §2-S4).
//!
//! Anchored shape: `code|name|keywords` per line, GB18030 text, CRLF
//! terminators. The keywords field may itself contain `|` (gen-2 has rows
//! with four fields); only the first two fields are bound, the remainder is
//! kept raw and never interpreted. GB18030 trail bytes can equal `|`, so the
//! member is decoded strictly before splitting — malformed text rejects the
//! whole file. One code mapping to more than one line is an identity
//! conflict, never a silent pick.

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExCodeRow {
    /// 1-based line number inside the member.
    pub line: usize,
    /// Field 0: six-digit code, leading zeroes preserved.
    pub code: String,
    /// Field 1: GB18030-decoded display name.
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExCodeError {
    #[error("infoharbor_ex.code is not valid GB18030")]
    Decode,
    #[error("infoharbor_ex.code line {line} is empty")]
    EmptyLine { line: usize },
    #[error("infoharbor_ex.code line {line} has {fields} fields, expected at least 3")]
    FieldCount { line: usize, fields: usize },
    #[error("infoharbor_ex.code line {line} code {value:?} is not exactly six digits")]
    BadCode { line: usize, value: String },
    #[error("infoharbor_ex.code line {line} has an empty name")]
    EmptyName { line: usize },
    #[error(
        "infoharbor_ex.code code {code:?} appears more than once (lines {first_line} and {line})"
    )]
    DuplicateCode {
        code: String,
        first_line: usize,
        line: usize,
    },
}

/// Parse the whole member into code→name rows. Duplicate codes reject the
/// file (the name source must be univalent to serve as a join key).
pub fn parse(bytes: &[u8]) -> Result<Vec<ExCodeRow>, ExCodeError> {
    let text = encoding_rs::GB18030
        .decode_without_bom_handling_and_without_replacement(bytes)
        .ok_or(ExCodeError::Decode)?;
    let mut seen = BTreeMap::<String, usize>::new();
    let mut rows = Vec::new();
    let mut lines = text.split("\r\n").enumerate().peekable();
    while let Some((index, line)) = lines.next() {
        let number = index + 1;
        if line.is_empty() {
            if lines.peek().is_none() {
                break;
            }
            return Err(ExCodeError::EmptyLine { line: number });
        }
        let fields: Vec<&str> = line.split('|').collect();
        if fields.len() < 3 {
            return Err(ExCodeError::FieldCount {
                line: number,
                fields: fields.len(),
            });
        }
        let code = fields[0];
        if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
            return Err(ExCodeError::BadCode {
                line: number,
                value: code.into(),
            });
        }
        let name = fields[1];
        if name.is_empty() {
            return Err(ExCodeError::EmptyName { line: number });
        }
        if let Some(first_line) = seen.insert(code.to_string(), number) {
            return Err(ExCodeError::DuplicateCode {
                code: code.into(),
                first_line,
                line: number,
            });
        }
        rows.push(ExCodeRow {
            line: number,
            code: code.to_string(),
            name: name.to_string(),
        });
    }
    Ok(rows)
}

/// Index rows by code. Parsing already rejects duplicates, so this is a
/// mechanical projection.
pub fn name_map(rows: &[ExCodeRow]) -> BTreeMap<&str, &str> {
    rows.iter()
        .map(|row| (row.code.as_str(), row.name.as_str()))
        .collect()
}
