//! Bounded `incon.dat` sectioned dictionary parser (design:
//! docs/tasks/issue-6-catalog-impl.md §2-S1/S5).
//!
//! Anchored shape: GB18030 text, CRLF terminators, `#SECTION` headers,
//! `code|name` entries under each header. Some sections carry extra `|`
//! separated columns (e.g. `id|group|label`); extra columns are retained in
//! `extra` and never interpreted. Duplicate codes inside one section reject
//! the file. The file's trailing `######` line parses as an empty section —
//! that is the retained shape, not a special case.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InconEntry {
    /// 1-based line number inside the member.
    pub line: usize,
    /// Field 0: code token (ASCII).
    pub code: String,
    /// Field 1: GB18030-decoded display name.
    pub name: String,
    /// Fields 2.. retained verbatim for sections whose rows carry more than
    /// `code|name`; consumers must not interpret them.
    pub extra: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InconSection {
    /// Header text without the leading `#`.
    pub name: String,
    pub entries: Vec<InconEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InconError {
    #[error("incon.dat is not valid GB18030")]
    Decode,
    #[error("incon.dat line {line} has content before the first section header")]
    EntryBeforeHeader { line: usize },
    #[error("incon.dat line {line} has {fields} fields, expected at least 2")]
    MalformedEntry { line: usize, fields: usize },
    #[error("incon.dat line {line} has a non-ASCII or empty code")]
    BadCode { line: usize },
    #[error("incon.dat line {line} has an empty name")]
    EmptyName { line: usize },
    #[error("incon.dat section {section:?} repeats code {code:?} (lines {first_line} and {line})")]
    DuplicateCode {
        section: String,
        code: String,
        first_line: usize,
        line: usize,
    },
}

/// Parse the whole member into ordered sections.
pub fn parse(bytes: &[u8]) -> Result<Vec<InconSection>, InconError> {
    let text = encoding_rs::GB18030
        .decode_without_bom_handling_and_without_replacement(bytes)
        .ok_or(InconError::Decode)?;
    let mut sections: Vec<InconSection> = Vec::new();
    let mut lines = text.split("\r\n").enumerate().peekable();
    while let Some((index, line)) = lines.next() {
        let number = index + 1;
        if line.is_empty() {
            if lines.peek().is_none() {
                break;
            }
            return Err(InconError::MalformedEntry {
                line: number,
                fields: 0,
            });
        }
        if let Some(header) = line.strip_prefix('#') {
            sections.push(InconSection {
                name: header.to_string(),
                entries: Vec::new(),
            });
            continue;
        }
        let section = sections
            .last_mut()
            .ok_or(InconError::EntryBeforeHeader { line: number })?;
        let fields: Vec<&str> = line.split('|').collect();
        if fields.len() < 2 {
            return Err(InconError::MalformedEntry {
                line: number,
                fields: fields.len(),
            });
        }
        let code = fields[0];
        if code.is_empty() || !code.is_ascii() {
            return Err(InconError::BadCode { line: number });
        }
        let name = fields[1];
        if name.is_empty() {
            return Err(InconError::EmptyName { line: number });
        }
        if section.entries.iter().any(|entry| entry.code == code) {
            let first_line = section
                .entries
                .iter()
                .find(|entry| entry.code == code)
                .map(|entry| entry.line)
                .unwrap_or(0);
            return Err(InconError::DuplicateCode {
                section: section.name.clone(),
                code: code.into(),
                first_line,
                line: number,
            });
        }
        section.entries.push(InconEntry {
            line: number,
            code: code.to_string(),
            name: name.to_string(),
            extra: fields[2..].iter().map(|field| field.to_string()).collect(),
        });
    }
    Ok(sections)
}

/// Anchored level formula (contract v1): `(len(code)-1)/2`, defined only for
/// an ASCII letter followed by an even number of digits — i.e. total length
/// 3, 5 or 7. Anything else is not a levelled code.
pub fn code_level(code: &str) -> Option<u8> {
    let bytes = code.as_bytes();
    if !matches!(bytes.len(), 3 | 5 | 7)
        || !bytes.first().is_some_and(u8::is_ascii_alphabetic)
        || !bytes[1..].iter().all(u8::is_ascii_digit)
    {
        return None;
    }
    Some(((bytes.len() - 1) / 2) as u8)
}

/// Derive the level-2 prefix of an `X` research-industry code: the code
/// itself when already level 2, its first five characters when level 3.
/// Level 1 codes and malformed inputs have no level-2 ancestor and yield
/// `None`; callers must not guess one.
pub fn research_level2(code: &str) -> Option<String> {
    match code_level(code) {
        Some(2) => Some(code.to_string()),
        Some(3) => Some(code[..5].to_string()),
        _ => None,
    }
}

/// Project one section into a code→name map. Parsing already rejects
/// duplicate codes inside a section.
pub fn section_map<'a>(
    sections: &'a [InconSection],
    name: &str,
) -> Option<Vec<(&'a str, &'a str)>> {
    sections
        .iter()
        .find(|section| section.name == name)
        .map(|section| {
            section
                .entries
                .iter()
                .map(|entry| (entry.code.as_str(), entry.name.as_str()))
                .collect()
        })
}
