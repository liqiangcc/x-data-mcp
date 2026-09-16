//! Bounded `tdxhy.cfg` line parser (design: docs/tasks/issue-6-catalog-impl.md §2-S2).
//!
//! Anchored shape (contract v1, #168): six `|` separated fields per line,
//! ASCII only, CRLF terminators. f0 is the market digit, f1 the six-digit
//! code with leading zeroes preserved, f2 a `T` industry code or empty,
//! f5 an `X` research-industry code or empty, f3/f4 always empty in this
//! generation. Any malformed line fails the whole file; no line is skipped
//! or repaired.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TdxhyRow {
    /// 1-based line number inside the member.
    pub line: usize,
    /// f0: market digit. Anchored enum is {0,1,2}; no exchange semantics are
    /// assigned here beyond the raw digit.
    pub market: u8,
    /// f1: six-digit security code, leading zeroes preserved.
    pub code: String,
    /// f2: `T`-prefixed industry code, or `None` when the field is empty.
    pub t_code: Option<String>,
    /// f5: `X`-prefixed research-industry code, or `None` when empty.
    pub x_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TdxhyError {
    #[error("tdxhy.cfg is not pure ASCII")]
    NotAscii,
    #[error("tdxhy.cfg line {line} is empty")]
    EmptyLine { line: usize },
    #[error("tdxhy.cfg line {line} has {fields} fields, expected 6")]
    FieldCount { line: usize, fields: usize },
    #[error("tdxhy.cfg line {line} f0 {value:?} is not a single digit 0..=2")]
    BadMarket { line: usize, value: String },
    #[error("tdxhy.cfg line {line} f1 {value:?} is not exactly six digits")]
    BadCode { line: usize, value: String },
    #[error("tdxhy.cfg line {line} f2 {value:?} is not a T code of anchored length")]
    BadIndustryCode { line: usize, value: String },
    #[error("tdxhy.cfg line {line} f5 {value:?} is not an X code of anchored length")]
    BadResearchCode { line: usize, value: String },
    #[error("tdxhy.cfg line {line} reserved field f{index} must be empty")]
    NonEmptyReservedField { line: usize, index: usize },
}

/// Anchored TDX code shape: one ASCII letter followed by 2, 4 or 6 digits, so
/// `(len-1)/2` yields level 1..=3 (contract v1 level formula).
pub(crate) fn code_shape(value: &str, letter: u8) -> bool {
    let bytes = value.as_bytes();
    matches!(bytes.len(), 3 | 5 | 7)
        && bytes.first() == Some(&letter)
        && bytes[1..].iter().all(u8::is_ascii_digit)
}

/// Parse the whole member. A single trailing empty element from the final
/// CRLF is tolerated; every other anomaly rejects the file.
pub fn parse(bytes: &[u8]) -> Result<Vec<TdxhyRow>, TdxhyError> {
    if !bytes.is_ascii() {
        return Err(TdxhyError::NotAscii);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| TdxhyError::NotAscii)?;
    let mut rows = Vec::new();
    let mut lines = text.split("\r\n").enumerate().peekable();
    while let Some((index, line)) = lines.next() {
        let number = index + 1;
        if line.is_empty() {
            if lines.peek().is_none() {
                break;
            }
            return Err(TdxhyError::EmptyLine { line: number });
        }
        let fields: Vec<&str> = line.split('|').collect();
        if fields.len() != 6 {
            return Err(TdxhyError::FieldCount {
                line: number,
                fields: fields.len(),
            });
        }
        let market = match fields[0] {
            "0" => 0u8,
            "1" => 1,
            "2" => 2,
            other => {
                return Err(TdxhyError::BadMarket {
                    line: number,
                    value: other.into(),
                });
            }
        };
        let code = fields[1];
        if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
            return Err(TdxhyError::BadCode {
                line: number,
                value: code.into(),
            });
        }
        let t_code = match fields[2] {
            "" => None,
            value if code_shape(value, b'T') => Some(value.to_string()),
            value => {
                return Err(TdxhyError::BadIndustryCode {
                    line: number,
                    value: value.into(),
                });
            }
        };
        for (index, value) in [(3usize, fields[3]), (4, fields[4])] {
            if !value.is_empty() {
                return Err(TdxhyError::NonEmptyReservedField {
                    line: number,
                    index,
                });
            }
        }
        let x_code = match fields[5] {
            "" => None,
            value if code_shape(value, b'X') => Some(value.to_string()),
            value => {
                return Err(TdxhyError::BadResearchCode {
                    line: number,
                    value: value.into(),
                });
            }
        };
        rows.push(TdxhyRow {
            line: number,
            market,
            code: code.to_string(),
            t_code,
            x_code,
        });
    }
    Ok(rows)
}
