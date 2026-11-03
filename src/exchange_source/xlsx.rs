//! Minimal XLSX reader for the exchange-source entry (M3-2 PR-A).
//!
//! The SZSE ETF list (`CATALOGID=1945`) is served as an `.xlsx` workbook, and
//! the crate deliberately carries no spreadsheet dependency: the workbook is a
//! zip with one sheet and inline/shared strings, so a small, strict reader is
//! enough and keeps the dependency surface unchanged.
//!
//! Scope: one sheet, cell values as text. Formatting, formulas, dates and
//! multiple sheets are not interpreted. A workbook that does not have the
//! expected shape fails closed with [`XlsxError`]; callers turn that into
//! `source_invalid` (design §6.3).
//!
//! The reader is used twice, from the same bytes:
//!
//! - [`crate::exchange_source::validate::parse_page`] counts the data rows for
//!   `exchange_source_snapshots`;
//! - the ETF identity adapter reads the `证券代码` column.
//!
//! Both calls are pure functions of the bytes, so replay can re-derive the
//! same counts without a network request.

use std::collections::BTreeMap;
use std::io::{Cursor, Read};

use zip::ZipArchive;

#[derive(Debug, thiserror::Error)]
pub enum XlsxError {
    #[error("xlsx archive: {0}")]
    Archive(String),
    #[error("xlsx entry {0} is missing")]
    Missing(String),
    #[error("xlsx entry {0}: {1}")]
    Entry(String, String),
    #[error("xlsx xml: {0}")]
    Xml(String),
}

/// One worksheet as rows of cell text. The first row is the header row when
/// the sheet has one; [`Sheet::data_rows`] excludes it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sheet {
    pub rows: Vec<Vec<String>>,
}

impl Sheet {
    /// Rows after the header row. An empty workbook has no data rows.
    pub fn data_rows(&self) -> usize {
        self.rows.len().saturating_sub(1)
    }
}

/// Read the first worksheet of the workbook.
pub fn read_first_sheet(bytes: &[u8]) -> Result<Sheet, XlsxError> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))
        .map_err(|error| XlsxError::Archive(error.to_string()))?;
    let shared = match read_entry(&mut archive, "xl/sharedStrings.xml") {
        Ok(text) => shared_strings(&text)?,
        Err(XlsxError::Missing(_)) => Vec::new(),
        Err(error) => return Err(error),
    };
    let sheet_name = sheet_name(&mut archive)?;
    let text = read_entry(&mut archive, &sheet_name)?;
    Ok(Sheet {
        rows: rows(&text, &shared)?,
    })
}

fn read_entry<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
    name: &str,
) -> Result<String, XlsxError> {
    let mut file = archive
        .by_name(name)
        .map_err(|_| XlsxError::Missing(name.to_string()))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| XlsxError::Entry(name.to_string(), error.to_string()))?;
    String::from_utf8(bytes).map_err(|error| XlsxError::Entry(name.to_string(), error.to_string()))
}

/// `sheet1.xml` when present, otherwise the first `xl/worksheets/*.xml` in
/// name order. A workbook without a worksheet fails closed.
fn sheet_name<R: Read + std::io::Seek>(archive: &mut ZipArchive<R>) -> Result<String, XlsxError> {
    if archive.by_name("xl/worksheets/sheet1.xml").is_ok() {
        return Ok("xl/worksheets/sheet1.xml".to_string());
    }
    let mut names: Vec<String> = (0..archive.len())
        .filter_map(|index| {
            archive
                .by_index(index)
                .ok()
                .map(|file| file.name().to_string())
        })
        .filter(|name| name.starts_with("xl/worksheets/") && name.ends_with(".xml"))
        .collect();
    names.sort();
    names
        .into_iter()
        .next()
        .ok_or_else(|| XlsxError::Missing("xl/worksheets/*.xml".to_string()))
}

/// Every `<si>` element's concatenated `<t>` text.
fn shared_strings(text: &str) -> Result<Vec<String>, XlsxError> {
    let mut out = Vec::new();
    for body in elements(text, "si")? {
        out.push(texts(&body, "t").join(""));
    }
    Ok(out)
}

/// Parse `<row>` elements into rows of cells.
fn rows(text: &str, shared: &[String]) -> Result<Vec<Vec<String>>, XlsxError> {
    let mut out = Vec::new();
    for body in elements(text, "row")? {
        out.push(row(&body, shared));
    }
    Ok(out)
}

fn row(body: &str, shared: &[String]) -> Vec<String> {
    let mut cells: BTreeMap<usize, String> = BTreeMap::new();
    let mut pos = 0;
    while let Some((start, open_end, self_closing)) = find_open(body, "c", pos) {
        let attrs = attributes(&body[start..open_end]);
        let column = attrs
            .get("r")
            .map(|reference| column_index(reference))
            .unwrap_or(0);
        let value = if self_closing {
            String::new()
        } else {
            let close = find_close(body, "c", open_end).unwrap_or(body.len());
            let inner = &body[open_end..close];
            match attrs.get("t").map(String::as_str).unwrap_or_default() {
                "inlineStr" => texts(inner, "t").join(""),
                "s" => texts(inner, "v")
                    .join("")
                    .trim()
                    .parse::<usize>()
                    .ok()
                    .and_then(|index| shared.get(index).cloned())
                    .unwrap_or_default(),
                _ => texts(inner, "v").join(""),
            }
        };
        cells.insert(column, value);
        pos = if self_closing {
            open_end
        } else {
            find_close(body, "c", open_end)
                .map(|close| close + "</c>".len())
                .unwrap_or(body.len())
        };
    }
    let last = cells.keys().next_back().copied().unwrap_or(0);
    (0..=last)
        .map(|index| cells.remove(&index).unwrap_or_default())
        .collect()
}

/// The inner bodies of every `<name>` element, in document order. Self-closing
/// elements contribute an empty body.
fn elements(text: &str, name: &str) -> Result<Vec<String>, XlsxError> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some((_, open_end, self_closing)) = find_open(text, name, pos) {
        if self_closing {
            out.push(String::new());
            pos = open_end;
            continue;
        }
        let close = find_close(text, name, open_end)
            .ok_or_else(|| XlsxError::Xml(format!("<{name}> at byte {pos} has no closing tag")))?;
        out.push(text[open_end..close].to_string());
        pos = close + name.len() + 3;
    }
    Ok(out)
}

/// Concatenated text of every `<name>` element inside `text`.
fn texts(text: &str, name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some((_, open_end, self_closing)) = find_open(text, name, pos) {
        if self_closing {
            pos = open_end;
            continue;
        }
        let Some(close) = find_close(text, name, open_end) else {
            break;
        };
        out.push(unescape(&text[open_end..close]));
        pos = close + name.len() + 3;
    }
    out
}

/// Find `<name ...>` at or after `from`: `(start of '<', byte after '>', self
/// closing)`. The name must end at whitespace, `>` or `/`, so `<row>` never
/// matches `<rows>`.
fn find_open(text: &str, name: &str, from: usize) -> Option<(usize, usize, bool)> {
    let mut search = from.min(text.len());
    while let Some(relative) = text[search..].find('<') {
        let start = search + relative;
        let after = start + 1;
        let rest = text.get(after..)?;
        if !rest.starts_with(name) {
            search = after;
            continue;
        }
        let boundary = rest[name.len()..].chars().next();
        if !matches!(boundary, Some(character) if character.is_whitespace() || character == '>' || character == '/')
        {
            search = after;
            continue;
        }
        let mut index = after + name.len();
        let mut quote: Option<char> = None;
        while index < text.len() {
            let character = text[index..].chars().next()?;
            match quote {
                Some(open) if character == open => quote = None,
                Some(_) => {}
                None if character == '"' || character == '\'' => quote = Some(character),
                None if character == '>' => break,
                None => {}
            }
            index += character.len_utf8();
        }
        if index >= text.len() {
            return None;
        }
        let self_closing = text[start..index].trim_end().ends_with('/');
        return Some((start, index + 1, self_closing));
    }
    None
}

fn find_close(text: &str, name: &str, from: usize) -> Option<usize> {
    text.get(from..)?
        .find(&format!("</{name}>"))
        .map(|relative| from + relative)
}

/// `name="value"` / `name='value'` pairs of an open tag (the `<name` prefix is
/// ignored). Entity references are unescaped.
fn attributes(tag: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let bytes = tag.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        while index < bytes.len() && !bytes[index].is_ascii_alphabetic() {
            index += 1;
        }
        let name_start = index;
        while index < bytes.len()
            && (bytes[index].is_ascii_alphanumeric()
                || bytes[index] == b'_'
                || bytes[index] == b':')
        {
            index += 1;
        }
        if name_start == index {
            break;
        }
        let name = tag[name_start..index].to_string();
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= bytes.len() || bytes[index] != b'=' {
            continue;
        }
        index += 1;
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= bytes.len() || (bytes[index] != b'"' && bytes[index] != b'\'') {
            continue;
        }
        let quote = bytes[index];
        index += 1;
        let value_start = index;
        while index < bytes.len() && bytes[index] != quote {
            index += 1;
        }
        out.insert(name, unescape(&tag[value_start..index]));
        index += 1;
    }
    out
}

/// `A1`/`BC12` → zero-based column index.
fn column_index(reference: &str) -> usize {
    let mut value = 0usize;
    for character in reference
        .chars()
        .take_while(|character| character.is_ascii_alphabetic())
    {
        value = value * 26 + (character.to_ascii_uppercase() as usize - 'A' as usize + 1);
    }
    value.saturating_sub(1)
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(index) = rest.find('&') {
        out.push_str(&rest[..index]);
        rest = &rest[index..];
        let Some(end) = rest.find(';') else {
            out.push_str(rest);
            return out;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| {
                    entity
                        .strip_prefix('#')
                        .and_then(|digits| digits.parse().ok())
                })
                .and_then(char::from_u32),
        };
        match decoded {
            Some(character) => out.push(character),
            None => out.push_str(&rest[..=end]),
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_letters_are_base_26() {
        assert_eq!(column_index("A1"), 0);
        assert_eq!(column_index("E770"), 4);
        assert_eq!(column_index("AA1"), 26);
    }

    #[test]
    fn entities_are_unescaped() {
        assert_eq!(unescape("a&amp;b&lt;c&#x4E2D;&#25991;"), "a&b<c中文");
    }

    #[test]
    fn a_missing_sheet_fails_closed() {
        let error = read_first_sheet(b"not a zip").unwrap_err();
        assert!(matches!(error, XlsxError::Archive(_)));
    }
}
