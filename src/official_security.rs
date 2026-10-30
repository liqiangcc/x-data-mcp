//! TDX official-file security facts (M2 PR-2, issue-391 §4/§5/§9.2/§16).
//!
//! This is the source adapter for the three members that carry security-master
//! facts:
//!
//! - `base.dbf` — the anchored 40-field table (#399 A-5). Only `SC`, `GPDM`,
//!   `ZGB`, `BG`, `HG`, `LTAG`, `SSDATE` are assigned meaning here; the other
//!   fields are preserved by the container parser but not interpreted.
//! - `infoharbor_spec.cfg` — `market|code|0|start|end` (#401 A-6). The third
//!   field is always `0` and is validated but not assigned a meaning.
//! - `gbbq` — categories 2/3/5/7/8/9/10 are capital facts (§5.1, D-50 R-3
//!   revision). Other categories are counted as excluded, never stored. For an
//!   admitted event only the after value and event day become state; the
//!   before value is kept as audit evidence (D-50).
//!
//! The adapter does not open a zip or a session: it takes the already
//! CRC-verified member bytes, so the same functions can be unit-tested without
//! a source. Daily-path wiring is PR-8 (§16); PR-2 delivers the parsed facts
//! and their writers.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;

use crate::dbf::{self, DbfRecord, FieldDescriptor};
use crate::gbbq;
use crate::security_master::{
    ArrangementFact, CapitalEvent, CapitalEventSet, CurrentShares, DelistingSource, Exchange,
    IssuerResolution, ListingSource, SecurityLifecycleFact, is_admitted_category,
};

pub mod a1a2;
pub mod a3;
pub mod risk_warnings;

/// One anchored field: name, dBase type byte, length, decimals (#399 A-5).
#[derive(Debug, Clone, Copy)]
struct FieldAnchor {
    name: &'static str,
    field_type: u8,
    length: u8,
    decimals: u8,
}

/// The complete 40-field `base.dbf` schema, in record order. Anchoring the
/// whole table (not only the fields we read) makes a vendor insert/remove or a
/// length change fail closed instead of silently shifting every later offset.
const ANCHORED_FIELDS: [FieldAnchor; 40] = [
    FieldAnchor {
        name: "SC",
        field_type: b'C',
        length: 1,
        decimals: 0,
    },
    FieldAnchor {
        name: "GPDM",
        field_type: b'C',
        length: 6,
        decimals: 0,
    },
    FieldAnchor {
        name: "GXRQ",
        field_type: b'N',
        length: 8,
        decimals: 0,
    },
    FieldAnchor {
        name: "ZGB",
        field_type: b'N',
        length: 14,
        decimals: 2,
    },
    FieldAnchor {
        name: "GJG",
        field_type: b'N',
        length: 14,
        decimals: 2,
    },
    FieldAnchor {
        name: "FQRFRG",
        field_type: b'N',
        length: 14,
        decimals: 2,
    },
    FieldAnchor {
        name: "FRG",
        field_type: b'N',
        length: 14,
        decimals: 2,
    },
    FieldAnchor {
        name: "BG",
        field_type: b'N',
        length: 14,
        decimals: 2,
    },
    FieldAnchor {
        name: "HG",
        field_type: b'N',
        length: 14,
        decimals: 2,
    },
    FieldAnchor {
        name: "LTAG",
        field_type: b'N',
        length: 14,
        decimals: 2,
    },
    FieldAnchor {
        name: "ZGG",
        field_type: b'N',
        length: 14,
        decimals: 2,
    },
    FieldAnchor {
        name: "ZPG",
        field_type: b'N',
        length: 14,
        decimals: 2,
    },
    FieldAnchor {
        name: "ZZC",
        field_type: b'N',
        length: 14,
        decimals: 1,
    },
    FieldAnchor {
        name: "LDZC",
        field_type: b'N',
        length: 14,
        decimals: 1,
    },
    FieldAnchor {
        name: "GDZC",
        field_type: b'N',
        length: 14,
        decimals: 1,
    },
    FieldAnchor {
        name: "WXZC",
        field_type: b'N',
        length: 14,
        decimals: 1,
    },
    FieldAnchor {
        name: "CQTZ",
        field_type: b'N',
        length: 14,
        decimals: 1,
    },
    FieldAnchor {
        name: "LDFZ",
        field_type: b'N',
        length: 14,
        decimals: 1,
    },
    FieldAnchor {
        name: "CQFZ",
        field_type: b'N',
        length: 14,
        decimals: 1,
    },
    FieldAnchor {
        name: "ZBGJJ",
        field_type: b'N',
        length: 14,
        decimals: 1,
    },
    FieldAnchor {
        name: "JZC",
        field_type: b'N',
        length: 14,
        decimals: 1,
    },
    FieldAnchor {
        name: "ZYSY",
        field_type: b'N',
        length: 14,
        decimals: 1,
    },
    FieldAnchor {
        name: "ZYLY",
        field_type: b'N',
        length: 14,
        decimals: 1,
    },
    FieldAnchor {
        name: "QTLY",
        field_type: b'N',
        length: 14,
        decimals: 1,
    },
    FieldAnchor {
        name: "YYLY",
        field_type: b'N',
        length: 14,
        decimals: 1,
    },
    FieldAnchor {
        name: "TZSY",
        field_type: b'N',
        length: 14,
        decimals: 1,
    },
    FieldAnchor {
        name: "BTSY",
        field_type: b'N',
        length: 14,
        decimals: 1,
    },
    FieldAnchor {
        name: "YYWSZ",
        field_type: b'N',
        length: 14,
        decimals: 1,
    },
    FieldAnchor {
        name: "SNSYTZ",
        field_type: b'N',
        length: 14,
        decimals: 1,
    },
    FieldAnchor {
        name: "LYZE",
        field_type: b'N',
        length: 14,
        decimals: 1,
    },
    FieldAnchor {
        name: "SHLY",
        field_type: b'N',
        length: 14,
        decimals: 1,
    },
    FieldAnchor {
        name: "JLY",
        field_type: b'N',
        length: 14,
        decimals: 1,
    },
    FieldAnchor {
        name: "WFPLY",
        field_type: b'N',
        length: 14,
        decimals: 1,
    },
    FieldAnchor {
        name: "TZMGJZ",
        field_type: b'N',
        length: 14,
        decimals: 3,
    },
    FieldAnchor {
        name: "DY",
        field_type: b'C',
        length: 3,
        decimals: 0,
    },
    FieldAnchor {
        name: "HY",
        field_type: b'C',
        length: 4,
        decimals: 0,
    },
    FieldAnchor {
        name: "ZBNB",
        field_type: b'N',
        length: 2,
        decimals: 0,
    },
    FieldAnchor {
        name: "SSDATE",
        field_type: b'C',
        length: 8,
        decimals: 0,
    },
    FieldAnchor {
        name: "MODIDATE",
        field_type: b'C',
        length: 6,
        decimals: 0,
    },
    FieldAnchor {
        name: "GDRS",
        field_type: b'N',
        length: 8,
        decimals: 0,
    },
];

const ANCHORED_HEADER_LENGTH: u16 = 32 + 40 * 32 + 1;
const ANCHORED_RECORD_LENGTH: u16 = 481;

const SC_INDEX: usize = 0;
const GPDM_INDEX: usize = 1;
const ZGB_INDEX: usize = 3;
const BG_INDEX: usize = 7;
const HG_INDEX: usize = 8;
const LTAG_INDEX: usize = 9;
const SSDATE_INDEX: usize = 37;

/// One `base.dbf` row with only the anchored fields decoded. Share values are
/// integer shares (the file stores 万股 at 2dp, #399 A-5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseDbfRow {
    /// `SC`: 0=SZ, 1=SH, 2=BJ, 9=anomaly (never an exchange).
    pub market: u8,
    pub code: String,
    /// `SSDATE`, subject-level listing date. `None` for the literal `'0'`
    /// sentinel, which is *missing*, not a delisting date.
    pub ssdate: Option<NaiveDate>,
    pub zgb: Option<i64>,
    pub bg: Option<i64>,
    pub hg: Option<i64>,
    pub ltag: Option<i64>,
}

/// One `(market, code)` repeated in `base.dbf` with **byte-identical**
/// records. The parser keeps the first record and names every collapsed
/// record index, so the main table's evidence can show what was dropped
/// (XDM-70 acceptance, #391).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollapsedDuplicate {
    /// `SC` as stored (0=SZ, 1=SH, 2=BJ, 9=anomaly).
    pub market: u8,
    pub code: String,
    /// 0-based record indices in the file, ascending; the first one is the
    /// record that was kept. Deleted records keep their physical index, so
    /// these numbers match the file's own record numbering.
    pub records: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseDbf {
    pub rows: Vec<BaseDbfRow>,
    pub deleted: usize,
    /// Byte-identical duplicates that were collapsed, in `(market, code)`
    /// order. A conflicting duplicate is still a hard error, never a row.
    pub duplicates_collapsed: Vec<CollapsedDuplicate>,
}

/// One `infoharbor_spec.cfg` row. `start`/`end` are the delisting-arrangement
/// bounds; `end` is the delisting-effective day and an open upper bound
/// (#401 A-6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InfoharborRow {
    pub market: u8,
    pub code: String,
    pub start: Option<NaiveDate>,
    pub end: Option<NaiveDate>,
}

/// The current `base.dbf` share snapshot, kept separately from the capital
/// events: it has no event day and must never be mistaken for one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentShareSnapshot {
    pub entity: String,
    pub shares: CurrentShares,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SkipCounts {
    pub deleted_base_rows: usize,
    pub non_a_share_base_rows: usize,
    pub market_mismatch_base_rows: usize,
    pub missing_listing_base_rows: usize,
    pub missing_listing_with_arrangement_rows: usize,
    pub non_a_share_arrangement_rows: usize,
    pub market_mismatch_arrangement_rows: usize,
    pub unmatched_arrangement_rows: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityFacts {
    pub lifecycle: Vec<SecurityLifecycleFact>,
    /// Every A-share `infoharbor_spec.cfg` row, including the codes whose
    /// `base.dbf` `SSDATE` is the `'0'` sentinel. The assembly needs the
    /// arrangement bounds for those exchange-only rows (#401 A-6).
    pub arrangements: Vec<ArrangementFact>,
    pub capital: Vec<CapitalEventSet>,
    pub current_shares: Vec<CurrentShareSnapshot>,
    pub skipped: SkipCounts,
    /// Byte-identical `base.dbf` repeats the parser collapsed (XDM-70
    /// acceptance, #391). Carried through so the first writer of the main
    /// table can report them in its item evidence.
    pub duplicates_collapsed: Vec<CollapsedDuplicate>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("base.dbf container: {0}")]
    Dbf(#[from] dbf::DbfError),
    #[error(
        "base.dbf header/record length drifted: expected header {expected_header}, record {expected_record}; found header {found_header}, record {found_record}"
    )]
    LengthDrift {
        expected_header: u16,
        expected_record: u16,
        found_header: u16,
        found_record: u16,
    },
    #[error("base.dbf field table drifted at index {index}: expected {expected}, found {found}")]
    SchemaDrift {
        index: usize,
        expected: String,
        found: String,
    },
    #[error(
        "base.dbf duplicates market {market} code {code} at records {first} and {second} with different bytes"
    )]
    BaseDuplicate {
        market: u8,
        code: String,
        first: usize,
        second: usize,
    },
    #[error("base.dbf record {record} field {field} is missing")]
    MissingField { record: usize, field: &'static str },
    #[error("base.dbf record {record} field {field} is not ASCII")]
    NonAsciiField { record: usize, field: &'static str },
    #[error("base.dbf record {record} field {field}: {detail}")]
    FieldValue {
        record: usize,
        field: &'static str,
        detail: String,
    },
    #[error("infoharbor_spec.cfg is not ASCII")]
    InfoharborNotAscii,
    #[error("infoharbor_spec.cfg line {line}: {detail}")]
    InfoharborLine { line: usize, detail: String },
    #[error("infoharbor_spec.cfg duplicates market {market} code {code}")]
    InfoharborDuplicate { market: u8, code: String },
    #[error("gbbq event {index} for {entity} has invalid date {date_raw}")]
    EventDate {
        index: usize,
        entity: String,
        date_raw: u32,
    },
    #[error("gbbq event {index} for {entity} has an ordinal beyond i64")]
    EventOrdinal { index: usize, entity: String },
    #[error(transparent)]
    Capital(#[from] crate::security_master::Error),
}

impl BaseDbf {
    /// Parse the container and anchor the complete 40-field schema.
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let file = dbf::parse(bytes)?;
        if file.header_length != ANCHORED_HEADER_LENGTH
            || file.record_length != ANCHORED_RECORD_LENGTH
        {
            return Err(Error::LengthDrift {
                expected_header: ANCHORED_HEADER_LENGTH,
                expected_record: ANCHORED_RECORD_LENGTH,
                found_header: file.header_length,
                found_record: file.record_length,
            });
        }
        if file.fields.len() != ANCHORED_FIELDS.len() {
            return Err(Error::SchemaDrift {
                index: file.fields.len().min(ANCHORED_FIELDS.len()),
                expected: format!("{} fields", ANCHORED_FIELDS.len()),
                found: format!("{} fields", file.fields.len()),
            });
        }
        for (index, (field, anchor)) in file.fields.iter().zip(ANCHORED_FIELDS.iter()).enumerate() {
            if field.name != anchor.name
                || field.field_type != anchor.field_type
                || field.length != anchor.length
                || field.decimals != anchor.decimals
            {
                return Err(Error::SchemaDrift {
                    index,
                    expected: format!(
                        "{} {}{}.{}",
                        anchor.name, anchor.field_type as char, anchor.length, anchor.decimals
                    ),
                    found: format!(
                        "{} {}{}.{}",
                        field.name, field.field_type as char, field.length, field.decimals
                    ),
                });
            }
        }

        let mut rows = Vec::with_capacity(file.records.len());
        let mut deleted = 0;
        // The key maps to the kept record's index and raw bytes: a repeat is
        // only collapsed when the bytes are identical, otherwise it fails
        // closed with both record numbers (never "take first/last").
        let mut seen: BTreeMap<(u8, String), (usize, &[u8])> = BTreeMap::new();
        let mut collapsed: BTreeMap<(u8, String), CollapsedDuplicate> = BTreeMap::new();
        for (record_index, record) in file.records.iter().enumerate() {
            if record.deleted {
                deleted += 1;
                continue;
            }
            let market = parse_market(
                field_text(record, &file.fields, SC_INDEX, record_index)?,
                record_index,
            )?;
            let code = parse_code(
                field_text(record, &file.fields, GPDM_INDEX, record_index)?,
                record_index,
            )?;
            let key = (market, code.clone());
            // `copied` keeps the map borrow out of the arms so the insert
            // compiles without a second lookup.
            match seen.get(&key).copied() {
                Some((first, raw)) if raw == record.raw.as_slice() => {
                    collapsed
                        .entry(key)
                        .or_insert_with(|| CollapsedDuplicate {
                            market,
                            code: code.clone(),
                            records: vec![first],
                        })
                        .records
                        .push(record_index);
                    continue;
                }
                Some((first, _)) => {
                    return Err(Error::BaseDuplicate {
                        market,
                        code,
                        first,
                        second: record_index,
                    });
                }
                None => {
                    seen.insert(key, (record_index, record.raw.as_slice()));
                }
            }
            let ssdate = parse_ssdate(
                field_text(record, &file.fields, SSDATE_INDEX, record_index)?,
                record_index,
            )?;
            let zgb = parse_shares(
                field_text(record, &file.fields, ZGB_INDEX, record_index)?,
                record_index,
                "ZGB",
            )?;
            let bg = parse_shares(
                field_text(record, &file.fields, BG_INDEX, record_index)?,
                record_index,
                "BG",
            )?;
            let hg = parse_shares(
                field_text(record, &file.fields, HG_INDEX, record_index)?,
                record_index,
                "HG",
            )?;
            let ltag = parse_shares(
                field_text(record, &file.fields, LTAG_INDEX, record_index)?,
                record_index,
                "LTAG",
            )?;
            rows.push(BaseDbfRow {
                market,
                code,
                ssdate,
                zgb,
                bg,
                hg,
                ltag,
            });
        }
        Ok(Self {
            rows,
            deleted,
            duplicates_collapsed: collapsed.into_values().collect(),
        })
    }
}

/// Parse the headerless, ASCII/CRLF `infoharbor_spec.cfg` (#401 A-6).
pub fn parse_infoharbor(bytes: &[u8]) -> Result<Vec<InfoharborRow>, Error> {
    if !bytes.is_ascii() {
        return Err(Error::InfoharborNotAscii);
    }
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' && (index == 0 || bytes[index - 1] != b'\r') {
            return Err(Error::InfoharborLine {
                line: bytes[..index].iter().filter(|b| **b == b'\n').count() + 1,
                detail: "LF without CR; the source is CRLF".into(),
            });
        }
        if *byte == b'\r' && bytes.get(index + 1) != Some(&b'\n') {
            return Err(Error::InfoharborLine {
                line: bytes[..index].iter().filter(|b| **b == b'\n').count() + 1,
                detail: "CR without LF; the source is CRLF".into(),
            });
        }
    }
    let text = std::str::from_utf8(bytes).map_err(|_| Error::InfoharborNotAscii)?;
    let body = text
        .strip_suffix("\r\n")
        .ok_or_else(|| Error::InfoharborLine {
            line: text.lines().count().max(1),
            detail: "file does not end with CRLF".into(),
        })?;
    let mut rows = Vec::new();
    let mut seen = BTreeSet::new();
    for (line_index, line) in body.split("\r\n").enumerate() {
        let line_number = line_index + 1;
        if line.is_empty() {
            return Err(Error::InfoharborLine {
                line: line_number,
                detail: "empty line".into(),
            });
        }
        let fields: Vec<&str> = line.split('|').collect();
        if fields.len() != 5 {
            return Err(Error::InfoharborLine {
                line: line_number,
                detail: format!("expected 5 fields, found {}", fields.len()),
            });
        }
        if fields[0].len() != 1 {
            return Err(Error::InfoharborLine {
                line: line_number,
                detail: format!("market is not one digit: {:?}", fields[0]),
            });
        }
        let market = fields[0].parse::<u8>().map_err(|_| Error::InfoharborLine {
            line: line_number,
            detail: format!("market is not a byte: {:?}", fields[0]),
        })?;
        if !matches!(market, 0..=2) {
            return Err(Error::InfoharborLine {
                line: line_number,
                detail: format!("market {market} is outside 0/1/2"),
            });
        }
        let code = fields[1];
        if code.len() != 6 || !code.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(Error::InfoharborLine {
                line: line_number,
                detail: format!("code is not six digits: {code:?}"),
            });
        }
        // The third field is always '0' in the verified source. It is a format
        // anchor, not a meaning: #401 did not anchor it, so it stays unused.
        if fields[2] != "0" {
            return Err(Error::InfoharborLine {
                line: line_number,
                detail: format!("third field is {:?}, expected \"0\"", fields[2]),
            });
        }
        let start = parse_optional_infoharbor_date(fields[3], line_number, "start")?;
        let end = parse_optional_infoharbor_date(fields[4], line_number, "end")?;
        if let (Some(start), Some(end)) = (start, end)
            && start > end
        {
            return Err(Error::InfoharborLine {
                line: line_number,
                detail: format!("start {start} is after end {end}"),
            });
        }
        if !seen.insert((market, code.to_string())) {
            return Err(Error::InfoharborDuplicate {
                market,
                code: code.to_string(),
            });
        }
        rows.push(InfoharborRow {
            market,
            code: code.to_string(),
            start,
            end,
        });
    }
    Ok(rows)
}

/// Assemble the TDX facts from the three parsed sources.
///
/// `base.dbf` is the row set; `infoharbor_spec.cfg` supplies the delisting
/// arrangement for a subset; `gbbq` supplies the capital events. Rows that
/// are not A shares (B/BJ/fund/bond/index) are counted and skipped, never
/// stored as `instrument_class=0`. A `base.dbf` row with the `SSDATE='0'`
/// sentinel gets no `security_master` interval: §4.1 has no defensible
/// `effective_from` for it, so PR-3's exchange branch owns those codes. The
/// full A-share arrangement subset is returned separately so PR-3 can attach
/// the 退市整理期 to an exchange-only row.
pub fn parse_security_facts(
    base: &BaseDbf,
    infoharbor: &[InfoharborRow],
    events: &[gbbq::Event],
) -> Result<SecurityFacts, Error> {
    let mut arrangement: BTreeMap<(u8, String), &InfoharborRow> = BTreeMap::new();
    for row in infoharbor {
        arrangement.insert((row.market, row.code.clone()), row);
    }
    let mut events_by_code: BTreeMap<(u8, String), Vec<&gbbq::Event>> = BTreeMap::new();
    for event in events {
        events_by_code
            .entry((event.market, event.code.clone()))
            .or_default()
            .push(event);
    }

    let mut used_arrangement = BTreeSet::new();
    let mut lifecycle = Vec::new();
    let mut capital = Vec::new();
    let mut current_shares = Vec::new();
    let mut skipped = SkipCounts {
        deleted_base_rows: base.deleted,
        ..SkipCounts::default()
    };

    for row in &base.rows {
        let Some(exchange) = classify_a_share(row.market, &row.code) else {
            if a_share_code_exchange(&row.code).is_some() {
                skipped.market_mismatch_base_rows += 1;
            } else {
                skipped.non_a_share_base_rows += 1;
            }
            continue;
        };
        let entity = format!("{}.{}", exchange.as_str(), row.code);
        let arrangement_row = arrangement.get(&(row.market, row.code.clone())).copied();
        if arrangement_row.is_some() {
            used_arrangement.insert((row.market, row.code.clone()));
        }
        current_shares.push(CurrentShareSnapshot {
            entity: entity.clone(),
            shares: CurrentShares {
                zgb: row.zgb,
                bg: row.bg,
                hg: row.hg,
                ltag: row.ltag,
            },
        });

        if let Some(listing) = row.ssdate {
            let delisting_date = arrangement_row.and_then(|row| row.end);
            let arrangement_start = arrangement_row.and_then(|row| row.start);
            let arrangement_end = arrangement_row.and_then(|row| row.end);
            lifecycle.push(SecurityLifecycleFact {
                exchange,
                code: row.code.clone(),
                effective_from: listing,
                effective_to: delisting_date,
                instrument_class: 0,
                issuer_key: None,
                b_listing_date: None,
                listing_date: Some(listing),
                listing_date_tdx: Some(listing),
                listing_source: ListingSource::TdxSsdate,
                delisting_date,
                delisting_date_tdx: delisting_date,
                delisting_source: if delisting_date.is_some() {
                    DelistingSource::TdxArrangement
                } else {
                    DelistingSource::Unknown
                },
                // §4.3: the reason class comes from the exchange list or an
                // announcement (PR-3). The arrangement bounds prove the period,
                // not the class, so this branch never guesses one.
                delisting_reason_class: crate::security_master::DelistingReason::Unknown,
                arrangement_start,
                arrangement_end,
                issuer_resolution: IssuerResolution::CodeDate,
                listing_conflict: false,
                delisting_conflict: false,
                known_at: None,
            });
        } else {
            skipped.missing_listing_base_rows += 1;
            if arrangement_row.is_some() {
                skipped.missing_listing_with_arrangement_rows += 1;
            }
        }

        capital.push(capital_set(
            &entity,
            events_by_code
                .get(&(row.market, row.code.clone()))
                .map(Vec::as_slice)
                .unwrap_or(&[]),
        )?);
    }

    for row in infoharbor {
        if used_arrangement.contains(&(row.market, row.code.clone())) {
            continue;
        }
        if classify_a_share(row.market, &row.code).is_some() {
            skipped.unmatched_arrangement_rows += 1;
        } else if a_share_code_exchange(&row.code).is_some() {
            skipped.market_mismatch_arrangement_rows += 1;
        } else {
            skipped.non_a_share_arrangement_rows += 1;
        }
    }

    // The arrangement list is the full A-share subset, not only the rows that
    // matched a `base.dbf` interval: the `SSDATE='0'` codes are exactly the
    // ones whose arrangement PR-3 attaches to an exchange-only row.
    let mut arrangements: Vec<ArrangementFact> = infoharbor
        .iter()
        .filter_map(|row| {
            let exchange = classify_a_share(row.market, &row.code)?;
            Some(ArrangementFact {
                exchange,
                code: row.code.clone(),
                start: row.start,
                end: row.end,
            })
        })
        .collect();
    arrangements.sort_by(|left, right| {
        (left.exchange, left.code.as_str()).cmp(&(right.exchange, right.code.as_str()))
    });

    Ok(SecurityFacts {
        lifecycle,
        arrangements,
        capital,
        current_shares,
        skipped,
        duplicates_collapsed: base.duplicates_collapsed.clone(),
    })
}

/// One entity's capital-event set from that entity's gbbq records (decode
/// order). [`parse_security_facts`] uses it for every `base.dbf` row; the
/// daily per-entity item (PR-8) uses it for the same entity from the same
/// session decode, so both paths produce byte-identical sets.
pub(crate) fn capital_set(entity: &str, events: &[&gbbq::Event]) -> Result<CapitalEventSet, Error> {
    let mut set = CapitalEventSet::empty(entity);
    for (index, event) in events.iter().enumerate() {
        if !is_admitted_category(event.category) {
            set.excluded += 1;
            *set.excluded_by_category.entry(event.category).or_insert(0) += 1;
            continue;
        }
        let effective_day = event_day(event.date_raw).ok_or_else(|| Error::EventDate {
            index,
            entity: entity.to_string(),
            date_raw: event.date_raw,
        })?;
        let ordinal = i64::try_from(set.events.len()).map_err(|_| Error::EventOrdinal {
            index,
            entity: entity.to_string(),
        })?;
        set.events.push(CapitalEvent::from_raw(
            ordinal,
            effective_day,
            event.category,
            event.c_raw.map(f32::to_bits),
        )?);
    }
    Ok(set)
}

/// A-share code segments. The exact list is a mechanical prefix fact; B
/// shares (200/900), funds, bonds and indices are deliberately absent.
fn a_share_code_exchange(code: &str) -> Option<Exchange> {
    if code.len() != 6 || !code.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    match &code[..3] {
        "600" | "601" | "603" | "605" | "688" | "689" => Some(Exchange::Sh),
        "000" | "001" | "002" | "003" | "300" | "301" | "302" => Some(Exchange::Sz),
        _ => None,
    }
}

/// The `SC`/market digit and the code segment must agree. A mismatch is
/// counted and skipped, not guessed: base.dbf also carries indices and funds
/// whose code segment belongs to the other exchange.
fn classify_a_share(market: u8, code: &str) -> Option<Exchange> {
    let exchange = Exchange::from_market(market)?;
    (a_share_code_exchange(code) == Some(exchange)).then_some(exchange)
}

fn field_text<'a>(
    record: &'a DbfRecord,
    fields: &[FieldDescriptor],
    index: usize,
    record_index: usize,
) -> Result<&'a str, Error> {
    let field = ANCHORED_FIELDS[index].name;
    let bytes = record
        .field_bytes(fields, index)
        .ok_or(Error::MissingField {
            record: record_index,
            field,
        })?;
    std::str::from_utf8(bytes).map_err(|_| Error::NonAsciiField {
        record: record_index,
        field,
    })
}

fn trim_field(text: &str) -> &str {
    text.trim_matches(|ch: char| ch == ' ' || ch == '\0')
}

fn parse_market(text: &str, record: usize) -> Result<u8, Error> {
    let text = trim_field(text);
    let byte = text.as_bytes();
    if byte.len() != 1 || !byte[0].is_ascii_digit() {
        return Err(Error::FieldValue {
            record,
            field: "SC",
            detail: format!("not a one-digit market: {text:?}"),
        });
    }
    Ok(byte[0] - b'0')
}

fn parse_code(text: &str, record: usize) -> Result<String, Error> {
    let text = trim_field(text);
    if text.len() != 6 || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(Error::FieldValue {
            record,
            field: "GPDM",
            detail: format!("not six digits: {text:?}"),
        });
    }
    Ok(text.to_string())
}

fn parse_ssdate(text: &str, record: usize) -> Result<Option<NaiveDate>, Error> {
    let text = trim_field(text);
    if text.is_empty() || text.chars().all(|ch| ch == '0') {
        return Ok(None);
    }
    parse_compact_date(text)
        .map(Some)
        .ok_or_else(|| Error::FieldValue {
            record,
            field: "SSDATE",
            detail: format!("not a real yyyymmdd date: {text:?}"),
        })
}

fn parse_shares(text: &str, record: usize, field: &'static str) -> Result<Option<i64>, Error> {
    let text = trim_field(text);
    if text.is_empty() {
        return Ok(None);
    }
    let (integer, fraction) = match text.split_once('.') {
        Some((integer, fraction)) => (integer, fraction),
        None => (text, ""),
    };
    if integer.is_empty()
        || !integer.bytes().all(|byte| byte.is_ascii_digit())
        || fraction.len() > 2
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(Error::FieldValue {
            record,
            field,
            detail: format!("not a non-negative 2dp 万股 amount: {text:?}"),
        });
    }
    let integer = integer.parse::<i64>().map_err(|_| Error::FieldValue {
        record,
        field,
        detail: format!("integer part out of range: {text:?}"),
    })?;
    let fraction = if fraction.is_empty() {
        0
    } else {
        format!("{fraction:0<2}")
            .parse::<i64>()
            .map_err(|_| Error::FieldValue {
                record,
                field,
                detail: format!("fraction part out of range: {text:?}"),
            })?
    };
    integer
        .checked_mul(10_000)
        .and_then(|shares| shares.checked_add(fraction * 100))
        .map(Some)
        .ok_or_else(|| Error::FieldValue {
            record,
            field,
            detail: format!("share value out of range: {text:?}"),
        })
}

fn parse_optional_infoharbor_date(
    text: &str,
    line: usize,
    field: &str,
) -> Result<Option<NaiveDate>, Error> {
    if text.is_empty() {
        return Ok(None);
    }
    parse_compact_date(text)
        .map(Some)
        .ok_or_else(|| Error::InfoharborLine {
            line,
            detail: format!("{field} is not a real yyyymmdd date: {text:?}"),
        })
}

fn parse_compact_date(text: &str) -> Option<NaiveDate> {
    if text.len() != 8 || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    NaiveDate::from_ymd_opt(
        text[..4].parse().ok()?,
        text[4..6].parse().ok()?,
        text[6..8].parse().ok()?,
    )
}

/// `yyyymmdd` (the GBBQ date encoding) → calendar date.
fn event_day(date_raw: u32) -> Option<NaiveDate> {
    NaiveDate::from_ymd_opt(
        i32::try_from(date_raw / 10000).ok()?,
        date_raw / 100 % 100,
        date_raw % 100,
    )
}

#[cfg(test)]
mod tests;
