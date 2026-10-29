//! `exchange_source_snapshots` storage (M3 PR-2, `#446` §9.2).
//!
//! The table is an **index** over the source store (PR-1a, §9.4.1): rows are
//! written only after the blob and its manifest line are durable, and replay
//! rebuilds the rows from the manifest without any HTTP request. Bytes never
//! enter the database (Q7), so `store_sha256` is stored as the ASCII hex digest
//! of the blob name — an explicit column, never a path.
//!
//! Layer position (separation points 1/2): this module only persists the
//! neutral [`SnapshotRow`]; source names, URLs and envelope dialects stay in
//! `crate::exchange_source`, and consumer parsers never appear here.

use rusqlite::{Connection, params};

use super::{integrity, storage};
use crate::exchange_source::SnapshotRow;
use crate::market_storage::Error;

const COLUMNS: &str = "snapshot_id,store_sha256,store_kind,source_id,url,request_fingerprint,\
     fetched_at,http_etag,http_last_modified,as_of,size_bytes,row_count,page_count,\
     parser_version,basis_status,notes";

type RawRow = (
    String,
    Vec<u8>,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    i64,
    i64,
    Option<i64>,
    String,
    String,
    Option<String>,
);

fn to_row(raw: RawRow) -> Result<SnapshotRow, Error> {
    Ok(SnapshotRow {
        snapshot_id: raw.0,
        store_sha256: hex_text(raw.1)?,
        store_kind: raw.2,
        source_id: raw.3,
        url: raw.4,
        request_fingerprint: raw.5,
        fetched_at: raw.6,
        http_etag: raw.7,
        http_last_modified: raw.8,
        as_of: raw.9,
        size_bytes: u64::try_from(raw.10)
            .map_err(|_| integrity(format!("negative exchange size_bytes {}", raw.10)))?,
        row_count: usize::try_from(raw.11)
            .map_err(|_| integrity(format!("negative exchange row_count {}", raw.11)))?,
        page_count: match raw.12 {
            Some(pages) => {
                Some(u32::try_from(pages).map_err(|_| {
                    integrity(format!("exchange page_count {pages} is out of range"))
                })?)
            }
            None => None,
        },
        parser_version: raw.13,
        basis_status: raw.14,
        notes: raw.15,
    })
}

fn hex_text(bytes: Vec<u8>) -> Result<String, Error> {
    String::from_utf8(bytes).map_err(|_| integrity("exchange store_sha256 is not ASCII hex"))
}

/// Every stored row, ordered by the primary key (deterministic summary input).
pub(crate) fn read(c: &Connection) -> Result<Vec<SnapshotRow>, Error> {
    let mut statement = c
        .prepare(&format!(
            "SELECT {COLUMNS} FROM exchange_source_snapshots ORDER BY snapshot_id"
        ))
        .map_err(storage)?;
    let rows = statement
        .query_map([], |r| {
            // Explicit column types: the tuple alias alone is too far away for
            // inference to be obvious at a glance.
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Vec<u8>>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, Option<String>>(8)?,
                r.get::<_, String>(9)?,
                r.get::<_, i64>(10)?,
                r.get::<_, i64>(11)?,
                r.get::<_, Option<i64>>(12)?,
                r.get::<_, String>(13)?,
                r.get::<_, String>(14)?,
                r.get::<_, Option<String>>(15)?,
            ))
        })
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    rows.into_iter().map(to_row).collect()
}

/// Insert one row unless its snapshot id is already stored. Returns whether a
/// row was inserted; a duplicate is a no-op (§6.3 "同指纹同 sha 不新增").
pub(crate) fn insert(c: &Connection, row: &SnapshotRow) -> Result<bool, Error> {
    let changed = c
        .execute(
            &format!(
                "INSERT OR IGNORE INTO exchange_source_snapshots({COLUMNS}) \
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)"
            ),
            params![
                row.snapshot_id.as_str(),
                row.store_sha256.as_bytes(),
                row.store_kind.as_str(),
                row.source_id.as_str(),
                row.url.as_str(),
                row.request_fingerprint.as_str(),
                row.fetched_at.as_str(),
                row.http_etag.as_deref(),
                row.http_last_modified.as_deref(),
                row.as_of.as_str(),
                row.size_bytes as i64,
                row.row_count as i64,
                row.page_count.map(i64::from),
                row.parser_version.as_str(),
                row.basis_status.as_str(),
                row.notes.as_deref(),
            ],
        )
        .map_err(storage)?;
    Ok(changed > 0)
}

/// Insert a frozen window's rows; returns how many were new.
pub(crate) fn insert_rows(c: &Connection, rows: &[SnapshotRow]) -> Result<usize, Error> {
    let mut inserted = 0;
    for row in rows {
        if insert(c, row)? {
            inserted += 1;
        }
    }
    Ok(inserted)
}

/// Replace the whole table from the store (replay `--scope exchange`).
pub(crate) fn replace(c: &Connection, rows: &[SnapshotRow]) -> Result<usize, Error> {
    c.execute("DELETE FROM exchange_source_snapshots", [])
        .map_err(storage)?;
    insert_rows(c, rows)
}
