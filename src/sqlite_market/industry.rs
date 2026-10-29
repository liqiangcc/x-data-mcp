//! Storage mapping for the M3 industry tables (design
//! `docs/tasks/issue-446-m3-delisted-industry.md` §5.1/§5.2/§9.2/§9.4.2).
//!
//! Two writers, one shape:
//!
//! - [`commit_industry_snapshot`] is the daily `derived:industry_snapshot`
//!   path: it appends one observation and extends the intervals forward. An
//!   observation that continues the newest run changes nothing (`no_change`);
//!   one that brings back a *previous* content opens a new interval, because a
//!   file that went A → B → A was changed twice (§5.3). A registration without
//!   a publisher time is not an observation at all: once the content is in the
//!   table it changes nothing (a copy proves only that we held the bytes).
//! - [`replace_industry`] is the `replay --scope industry` path: the source
//!   store is authoritative, so the whole table set is recomputed and written
//!   in one transaction. A DB snapshot the store does not hold is
//!   `source_store_index_orphan` and is refused (§9.4.1).
//!
//! The interval computation itself lives in [`crate::industry`]; this module
//! only maps it onto SQLite. Bytes never enter these tables (Q7): a snapshot
//! row is an index over content-addressed store bytes, and `store_sha256` must
//! equal `member_sha256` for a `tdxhy.cfg` extraction (§9.2 note).
//!
//! Write ordering (§9.4.1) is the caller's: the source-store registration
//! happens before this transaction is opened. Nothing here can be reached
//! before the blob is durable.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Connection, OptionalExtension, params};

use super::{integrity, storage};
use crate::industry::{
    self, AssignmentRow, IndustryMember, SnapshotFacts, SnapshotRecord, SourceKind,
    parse_dictionary, parse_members,
};
use crate::market_storage::Error;
use crate::source_store::{self, ManifestRecord, SourceStore, StoreKind};

/// One daily-path write: the snapshot facts the item acquired, and the content
/// of the observation that came before it in the source store (the item knows
/// it: it just appended its own registration there).
pub(crate) struct IndustrySnapshotWrite<'a> {
    pub facts: &'a SnapshotFacts,
    pub previous_member_sha256: Option<&'a str>,
}

/// What the item reports, and what the replay verifies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IndustryCommit {
    /// The observation changed nothing: the intervals and the stored snapshot
    /// row already said exactly this.
    pub no_change: bool,
    pub snapshot_rows: i64,
    pub assignment_rows: i64,
    /// Same-day collisions resolved by dropping a zero-length segment.
    pub superseded: usize,
}

const SNAPSHOT_COLUMNS: &str = "member_sha256,store_sha256,base_zip_sha256,\
generation_manifest_sha256,source_kind,publisher_last_modified,observed_at,ingested_at,\
evidence_grade,parser_version,row_count,scope_member_count,dictionary_sha256,notes";

const ASSIGNMENT_COLUMNS: &str = "exchange,code,level,industry_code,name,name_status,\
effective_from,effective_to,known_at,effective_from_basis,evidence_grade,t_code,\
snapshot_member_sha256";

fn snapshot_placeholders() -> String {
    (1..=14)
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// The daily item's write: append one snapshot and extend the intervals.
pub(crate) fn commit_industry_snapshot(
    c: &Connection,
    write: &IndustrySnapshotWrite<'_>,
) -> Result<IndustryCommit, Error> {
    let facts = write.facts;
    validate(facts)?;
    // Whether this observation continues the newest run is a fact about the
    // observation sequence, not about the table: a content that came back after
    // a different one must open a new interval even though its `member_sha256`
    // is already stored (§5.3).
    let link = if write.previous_member_sha256 == Some(facts.record.member_sha256.as_str()) {
        industry::RunLink::Continue
    } else {
        industry::RunLink::Fresh
    };
    let stored = read_assignments(c)?;
    let (rows, superseded) = industry::apply_on(&stored, facts, link)
        .map_err(|error| Error::Integrity(error.to_string()))?;
    // §5.3 step 1 for storage: one row per content, its evidence merged over
    // every registration. The row is only rewritten when the merge actually
    // improves a stored column.
    let existing = read_snapshot(c, &facts.record.member_sha256)?;
    let merged = existing
        .as_ref()
        .map(|stored| stored.merged_with(&facts.record));
    let snapshot_changed = match (&existing, &merged) {
        (Some(stored), Some(merged)) => stored_row(stored) != stored_row(merged),
        _ => true,
    };
    if rows == stored && !snapshot_changed {
        // A repeated observation inside one run: the intervals keep their
        // evidence and the snapshot row is already the merge of both.
        return Ok(IndustryCommit {
            no_change: true,
            snapshot_rows: count(c, "SELECT count(*) FROM industry_snapshots")?,
            assignment_rows: count(c, "SELECT count(*) FROM industry_assignments")?,
            superseded: 0,
        });
    }
    match merged {
        Some(merged) => update_snapshot(c, &merged)?,
        None => insert_snapshot(c, &facts.record)?,
    }
    if rows != stored {
        c.execute("DELETE FROM industry_assignments", [])
            .map_err(storage)?;
        for row in &rows {
            insert_assignment(c, row)?;
        }
    }
    Ok(IndustryCommit {
        no_change: false,
        snapshot_rows: count(c, "SELECT count(*) FROM industry_snapshots")?,
        assignment_rows: count(c, "SELECT count(*) FROM industry_assignments")?,
        superseded,
    })
}

/// The row as it reads back: `imported_at` has no column (it is the
/// registration's, not the content's), so it never counts as a change.
fn stored_row(record: &SnapshotRecord) -> SnapshotRecord {
    SnapshotRecord {
        imported_at: None,
        ..record.clone()
    }
}

fn read_snapshot(c: &Connection, member_sha256: &str) -> Result<Option<SnapshotRecord>, Error> {
    let mut statement = c
        .prepare(&format!(
            "SELECT {SNAPSHOT_COLUMNS} FROM industry_snapshots WHERE member_sha256=?1"
        ))
        .map_err(storage)?;
    statement
        .query_row(params![blob(member_sha256)?], snapshot_from_row)
        .optional()
        .map_err(storage)
}

/// §5.3 step 1 for storage: rewrite the row a later registration of the same
/// content may improve. Every column is rewritten from the merged record — not
/// just the four time/grade columns — so the daily path and a replay end up with
/// the same row when the merge moves `source_kind` or the container/manifest
/// fields to an earlier registration. The primary key cannot change (and the
/// interval rows reference it), so this is an in-place update.
fn update_snapshot(c: &Connection, record: &SnapshotRecord) -> Result<(), Error> {
    let sql = format!(
        "UPDATE industry_snapshots SET {} WHERE member_sha256=?1",
        SNAPSHOT_COLUMNS
            .split(',')
            .enumerate()
            .skip(1)
            .map(|(index, column)| format!("{column}=?{}", index + 1))
            .collect::<Vec<_>>()
            .join(",")
    );
    c.execute(
        &sql,
        params![
            blob(&record.member_sha256)?,
            blob(&record.store_sha256)?,
            optional_blob(record.base_zip_sha256.as_deref())?,
            optional_blob(record.generation_manifest_sha256.as_deref())?,
            record.source_kind.as_str(),
            record.publisher_last_modified,
            record.observed_at,
            record.ingested_at,
            record.evidence_grade,
            record.parser_version,
            record.row_count,
            record.scope_member_count,
            optional_blob(record.dictionary_sha256.as_deref())?,
            record.notes,
        ],
    )
    .map_err(storage)?;
    Ok(())
}

/// The replay write: the store's snapshot set is authoritative. Any index row
/// without a store registration is refused instead of being silently dropped.
pub(crate) fn replace_industry(
    c: &Connection,
    rebuild: &industry::Rebuild,
) -> Result<IndustryCommit, Error> {
    let expected: BTreeSet<&str> = rebuild
        .snapshots
        .iter()
        .map(|snapshot| snapshot.member_sha256.as_str())
        .collect();
    let stored = read_snapshots(c)?;
    let orphans: Vec<&str> = stored
        .iter()
        .map(|snapshot| snapshot.member_sha256.as_str())
        .filter(|sha| !expected.contains(sha))
        .collect();
    if !orphans.is_empty() {
        return Err(Error::Integrity(format!(
            "source_store_index_orphan: industry_snapshots has {} row(s) the source store does \
             not: {}",
            orphans.len(),
            orphans
                .iter()
                .take(20)
                .copied()
                .collect::<Vec<_>>()
                .join(",")
        )));
    }
    for snapshot in &rebuild.snapshots {
        validate_record(snapshot)?;
    }
    // Assignments reference the snapshot rows, so they go first; the snapshot
    // rows are then replaced wholesale (their evidence can change on replay
    // without changing the intervals).
    c.execute("DELETE FROM industry_assignments", [])
        .map_err(storage)?;
    c.execute("DELETE FROM industry_snapshots", [])
        .map_err(storage)?;
    for snapshot in &rebuild.snapshots {
        insert_snapshot(c, snapshot)?;
    }
    for row in &rebuild.assignments {
        insert_assignment(c, row)?;
    }
    Ok(IndustryCommit {
        no_change: false,
        snapshot_rows: count(c, "SELECT count(*) FROM industry_snapshots")?,
        assignment_rows: count(c, "SELECT count(*) FROM industry_assignments")?,
        superseded: rebuild.superseded,
    })
}

/// Stored snapshot rows in `(ingested_at, member_sha256)` order. `imported_at`
/// is not a column (schema 25): the backfill knowledge time already shows up as
/// the interval rows' `known_at`, and the source store's manifest remains the
/// authority for when the bytes arrived.
pub(crate) fn read_snapshots(c: &Connection) -> Result<Vec<SnapshotRecord>, Error> {
    let mut statement = c
        .prepare(&format!(
            "SELECT {SNAPSHOT_COLUMNS} FROM industry_snapshots ORDER BY ingested_at, member_sha256"
        ))
        .map_err(storage)?;
    let rows = statement
        .query_map([], snapshot_from_row)
        .map_err(storage)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(storage)
}

fn snapshot_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SnapshotRecord> {
    let source_kind: String = row.get(4)?;
    let source_kind = SourceKind::parse(&source_kind).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            4,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::other(format!(
                "industry_snapshots.source_kind {source_kind:?}"
            ))),
        )
    })?;
    let member_sha256: Vec<u8> = row.get(0)?;
    let store_sha256: Vec<u8> = row.get(1)?;
    Ok(SnapshotRecord {
        member_sha256: hex(member_sha256),
        store_sha256: hex(store_sha256),
        base_zip_sha256: optional_hex(row.get(2)?),
        generation_manifest_sha256: optional_hex(row.get(3)?),
        source_kind,
        publisher_last_modified: row.get(5)?,
        observed_at: row.get(6)?,
        ingested_at: row.get(7)?,
        imported_at: None,
        evidence_grade: row.get(8)?,
        parser_version: row.get(9)?,
        row_count: row.get(10)?,
        scope_member_count: row.get(11)?,
        dictionary_sha256: optional_hex(row.get(12)?),
        notes: row.get(13)?,
    })
}

/// Stored interval rows in primary-key order.
pub(crate) fn read_assignments(c: &Connection) -> Result<Vec<AssignmentRow>, Error> {
    let mut statement = c
        .prepare(&format!(
            "SELECT {ASSIGNMENT_COLUMNS} FROM industry_assignments \
             ORDER BY exchange, code, effective_from"
        ))
        .map_err(storage)?;
    let rows = statement
        .query_map([], |row| {
            let snapshot: Vec<u8> = row.get(12)?;
            Ok(AssignmentRow {
                exchange: row.get(0)?,
                code: row.get(1)?,
                level: row.get(2)?,
                industry_code: row.get(3)?,
                name: row.get(4)?,
                name_status: row.get(5)?,
                effective_from: row.get(6)?,
                effective_to: row.get(7)?,
                known_at: row.get(8)?,
                basis: row.get(9)?,
                evidence_grade: row.get(10)?,
                t_code: row.get(11)?,
                snapshot_member_sha256: hex(snapshot),
            })
        })
        .map_err(storage)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(storage)
}

/// The source-store replay body: parse every registered `tdxhy.cfg` (and the
/// `incon.dat` of the same source) into snapshot facts, then rebuild the
/// intervals deterministically. Every blob is read back by digest first, so a
/// corrupt store fails the whole scope instead of producing half a table.
pub(crate) fn facts_from_store(store: &SourceStore) -> Result<Vec<SnapshotFacts>, Error> {
    let records = store
        .records()
        .map_err(|error| Error::Integrity(error.to_string()))?;
    // Every registration is an observation (§5.3): the same bytes observed twice
    // are two points in the timeline, even though storage keeps one row. Parsing
    // is content addressed, so the duplicates share one parse.
    let mut parsed: BTreeMap<String, (i64, Vec<IndustryMember>)> = BTreeMap::new();
    let mut dictionaries: BTreeMap<String, Option<BTreeMap<String, String>>> = BTreeMap::new();
    let mut snapshots: Vec<SnapshotFacts> = Vec::new();
    for record in &records {
        if record.source_id != source_store::TDXHY_SOURCE_ID {
            continue;
        }
        let (row_count, members) = match parsed.get(&record.sha256) {
            Some(cached) => cached.clone(),
            None => {
                let bytes = store
                    .read_verified(&record.sha256)
                    .map_err(|error| Error::Integrity(error.to_string()))?;
                let value =
                    parse_members(&bytes).map_err(|error| Error::Integrity(error.to_string()))?;
                parsed.insert(record.sha256.clone(), value.clone());
                value
            }
        };
        let paired = dictionary_for(store, &records, record, &mut dictionaries)?;
        snapshots.push(SnapshotFacts {
            record: snapshot_record(record, row_count, members.len(), paired.sha256)?,
            members,
            dictionary_present: paired.names.is_some(),
            dictionary: paired.names.unwrap_or_default(),
        });
    }
    Ok(snapshots)
}

/// The dictionary a snapshot pairs with: the parsed names (absent when the
/// paired `incon.dat` has no `#TDXRSHY` section) and the sha256 of its bytes,
/// or `None`/`None` when the same source registered no dictionary at all.
struct PairedDictionary {
    names: Option<BTreeMap<String, String>>,
    sha256: Option<String>,
}

/// The `incon.dat` registered by the same source as `tdxhy` (§5.6).
fn dictionary_for(
    store: &SourceStore,
    records: &[ManifestRecord],
    tdxhy: &ManifestRecord,
    cache: &mut BTreeMap<String, Option<BTreeMap<String, String>>>,
) -> Result<PairedDictionary, Error> {
    let absent = PairedDictionary {
        names: None,
        sha256: None,
    };
    let Some(key) = pairing_key(&tdxhy.origin) else {
        return Ok(absent);
    };
    let sibling = records.iter().find(|record| {
        record.source_id == source_store::INCON_SOURCE_ID
            && pairing_key(&record.origin).as_deref() == Some(key.as_str())
    });
    let Some(sibling) = sibling else {
        return Ok(absent);
    };
    let names = match cache.get(&sibling.sha256) {
        Some(cached) => cached.clone(),
        None => {
            let bytes = store
                .read_verified(&sibling.sha256)
                .map_err(|error| Error::Integrity(error.to_string()))?;
            let names =
                parse_dictionary(&bytes).map_err(|error| Error::Integrity(error.to_string()))?;
            cache.insert(sibling.sha256.clone(), names.clone());
            names
        }
    };
    Ok(PairedDictionary {
        names,
        sha256: Some(sibling.sha256.clone()),
    })
}

/// The pairing key an origin names, so a `tdxhy.cfg` and the `incon.dat` from
/// the same archive batch pair up. A generation registers both under
/// `generation:<manifest sha256>`; an archive import under
/// `<batch>:<container directory>`.
fn pairing_key(origin: &str) -> Option<String> {
    if origin.starts_with("generation:") {
        return Some(origin.to_string());
    }
    let (batch, rest) = origin.split_once(':')?;
    let container = rest
        .strip_suffix("!tdxhy.cfg")
        .or_else(|| rest.strip_suffix("!zhb.zip!incon.dat"))?;
    let directory = container
        .rsplit_once('/')
        .map(|(directory, _)| directory)
        .unwrap_or("");
    Some(format!("{batch}:{directory}"))
}

/// Project one manifest registration into the snapshot metadata the interval
/// computation consumes (§5.1). An unsupported `parser_version` is refused
/// instead of being replayed by a parser it was not written for (§9.4.1).
fn snapshot_record(
    record: &ManifestRecord,
    row_count: i64,
    scope_member_count: usize,
    dictionary_sha256: Option<String>,
) -> Result<SnapshotRecord, Error> {
    let parser_version = record
        .parser_version
        .clone()
        .ok_or_else(|| Error::Integrity(format!("{} has no parser_version", record.sha256)))?;
    if !source_store::known_parser_version(&parser_version) {
        return Err(Error::Integrity(format!(
            "unknown_parser: {} was registered with {parser_version}",
            record.sha256
        )));
    }
    let source_kind = match record.store_kind {
        StoreKind::GenerationMember => SourceKind::Generation,
        StoreKind::ArchiveImport => SourceKind::Archive,
        StoreKind::RepoEvidence => SourceKind::RepoEvidence,
        StoreKind::HttpFetch => {
            return Err(Error::Integrity(format!(
                "{} is an HTTP fetch, not an industry member",
                record.sha256
            )));
        }
    };
    let evidence_grade = record
        .evidence_grade
        .clone()
        .unwrap_or_else(|| default_grade(source_kind, record));
    if !matches!(evidence_grade.as_str(), "A" | "B" | "C") {
        return Err(Error::Integrity(format!(
            "{} has evidence grade {evidence_grade:?}",
            record.sha256
        )));
    }
    Ok(SnapshotRecord {
        member_sha256: record.sha256.clone(),
        store_sha256: record.sha256.clone(),
        base_zip_sha256: record.container_sha256.clone(),
        generation_manifest_sha256: generation_manifest(&record.origin),
        source_kind,
        publisher_last_modified: record.publisher_last_modified.clone(),
        // `observed_at` is when the bytes were fetched (an archive index keeps
        // the historical fetch time); `ingested_at` is when *this system* first
        // held them, i.e. the import time for a backfill (§5.2).
        observed_at: Some(record.acquired_at.clone()),
        ingested_at: record
            .imported_at
            .clone()
            .unwrap_or_else(|| record.acquired_at.clone()),
        imported_at: record.imported_at.clone(),
        evidence_grade,
        parser_version,
        row_count,
        scope_member_count: i64::try_from(scope_member_count)
            .map_err(|_| integrity("scope member count overflow"))?,
        dictionary_sha256,
        notes: None,
    })
}

fn default_grade(source_kind: SourceKind, record: &ManifestRecord) -> String {
    match source_kind {
        SourceKind::RepoEvidence => "C".to_string(),
        _ if record
            .publisher_last_modified
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty()) =>
        {
            "A".to_string()
        }
        _ => "B".to_string(),
    }
}

/// `generation:<manifest sha256>` origins carry the generation the member came
/// from; an archive import has none.
fn generation_manifest(origin: &str) -> Option<String> {
    origin
        .strip_prefix("generation:")
        .filter(|value| source_store::validate_sha256(value).is_ok())
        .map(str::to_string)
}

fn validate(facts: &SnapshotFacts) -> Result<(), Error> {
    validate_record(&facts.record)?;
    if !facts.dictionary_present && !facts.dictionary.is_empty() {
        return Err(integrity(
            "an absent dictionary must not supply industry names",
        ));
    }
    for member in &facts.members {
        if member.level != 2 {
            return Err(integrity(format!(
                "industry assignment level {} is not the level-2 scheme",
                member.level
            )));
        }
    }
    Ok(())
}

fn validate_record(record: &SnapshotRecord) -> Result<(), Error> {
    blob(&record.member_sha256)?;
    blob(&record.store_sha256)?;
    if record.member_sha256 != record.store_sha256 {
        return Err(integrity(format!(
            "snapshot {} stores {}: a tdxhy extraction is content addressed, so the two must match",
            record.member_sha256, record.store_sha256
        )));
    }
    for value in [
        record.base_zip_sha256.as_deref(),
        record.generation_manifest_sha256.as_deref(),
        record.dictionary_sha256.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        blob(value)?;
    }
    if !matches!(record.evidence_grade.as_str(), "A" | "B" | "C") {
        return Err(integrity(format!(
            "evidence grade {:?} is not A/B/C",
            record.evidence_grade
        )));
    }
    if record.parser_version.is_empty() {
        return Err(integrity(
            "a snapshot must name the parser version it was read with",
        ));
    }
    Ok(())
}

fn insert_snapshot(c: &Connection, record: &SnapshotRecord) -> Result<(), Error> {
    let sql = format!(
        "INSERT OR REPLACE INTO industry_snapshots({SNAPSHOT_COLUMNS}) VALUES({})",
        snapshot_placeholders()
    );
    c.execute(
        &sql,
        params![
            blob(&record.member_sha256)?,
            blob(&record.store_sha256)?,
            optional_blob(record.base_zip_sha256.as_deref())?,
            optional_blob(record.generation_manifest_sha256.as_deref())?,
            record.source_kind.as_str(),
            record.publisher_last_modified,
            record.observed_at,
            record.ingested_at,
            record.evidence_grade,
            record.parser_version,
            record.row_count,
            record.scope_member_count,
            optional_blob(record.dictionary_sha256.as_deref())?,
            record.notes,
        ],
    )
    .map_err(storage)?;
    Ok(())
}

fn insert_assignment(c: &Connection, row: &AssignmentRow) -> Result<(), Error> {
    let sql = format!(
        "INSERT INTO industry_assignments({ASSIGNMENT_COLUMNS},scheme) VALUES(?1,?2,?3,?4,?5,\
         ?6,?7,?8,?9,?10,?11,?12,?13,?14)"
    );
    c.execute(
        &sql,
        params![
            row.exchange,
            row.code,
            row.level,
            row.industry_code,
            row.name,
            row.name_status,
            row.effective_from,
            row.effective_to,
            row.known_at,
            row.basis,
            row.evidence_grade,
            row.t_code,
            blob(&row.snapshot_member_sha256)?,
            industry::SCHEME,
        ],
    )
    .map_err(storage)?;
    Ok(())
}

fn count(c: &Connection, sql: &str) -> Result<i64, Error> {
    c.query_row(sql, [], |row| row.get(0)).map_err(storage)
}

/// 32-byte blob of a lowercase hex digest; anything else is refused before it
/// can become a stored value (the columns are `BLOB`, so a typo would otherwise
/// silently store text).
fn blob(value: &str) -> Result<Vec<u8>, Error> {
    source_store::validate_sha256(value)
        .map_err(|error| integrity(format!("{value:?}: {error}")))?;
    let bytes = (0..value.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&value[index..index + 2], 16))
        .collect::<Result<Vec<u8>, _>>()
        .map_err(|_| integrity(format!("{value:?} is not hex")))?;
    if bytes.len() != 32 {
        return Err(integrity(format!("{value:?} is not a 32-byte digest")));
    }
    Ok(bytes)
}

fn optional_blob(value: Option<&str>) -> Result<Option<Vec<u8>>, Error> {
    value.map(blob).transpose()
}

fn hex(bytes: Vec<u8>) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn optional_hex(value: Option<Vec<u8>>) -> Option<String> {
    value.map(hex)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_key_pairs_a_generation_and_an_archive_batch() {
        assert_eq!(
            pairing_key("generation:abc").as_deref(),
            Some("generation:abc")
        );
        assert_eq!(
            pairing_key("gen-20261024:2026-09-30/base.zip!tdxhy.cfg").as_deref(),
            Some("gen-20261024:2026-09-30")
        );
        assert_eq!(
            pairing_key("gen-20261024:2026-09-30/gbbq.zip!zhb.zip!incon.dat").as_deref(),
            Some("gen-20261024:2026-09-30")
        );
        assert_eq!(pairing_key("unrelated:file.bin"), None);
    }

    #[test]
    fn generation_manifest_reads_only_valid_origins() {
        assert_eq!(generation_manifest("generation:abc"), None);
        let sha = "a".repeat(64);
        assert_eq!(
            generation_manifest(&format!("generation:{sha}")).as_deref(),
            Some(sha.as_str())
        );
    }

    #[test]
    fn blob_refuses_short_or_non_hex_values() {
        assert!(blob("abc").is_err());
        assert!(blob(&"z".repeat(64)).is_err());
        assert_eq!(blob(&"a".repeat(64)).unwrap(), vec![0xaa; 32]);
    }

    // --- database round trip: daily path, fresh-init replay, backfill -------

    use std::path::PathBuf;

    use crate::industry::{GenerationMembers, Rebuild};
    use crate::source_store::{Registration, SourceStore, StoreKind};
    use crate::sqlite_market::backend::MarketBackend;
    use crate::sqlite_tasks::SqliteTasks;

    const DICTIONARY: &[u8] = b"#TDXRSHY\r\nX5001|\xc8\xed\xbc\xfe\xb7\xfe\xce\xf1\r\n";

    fn cfg(entries: &[(u8, &str, &str, &str)]) -> Vec<u8> {
        let mut out = String::new();
        for (market, code, t_code, x_code) in entries {
            out.push_str(&format!("{market}|{code}|{t_code}|||{x_code}\r\n"));
        }
        out.into_bytes()
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        path: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("market.db");
            SqliteTasks::new(&path).initialize_compact().unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
            Self { _dir: dir, path }
        }

        fn store(&self) -> SourceStore {
            SourceStore::new(self._dir.path().join("source-store"))
        }

        fn backend(&self) -> MarketBackend {
            MarketBackend::open(&SqliteTasks::new(&self.path)).unwrap()
        }

        fn clear_industry(&self) {
            let c = rusqlite::Connection::open(&self.path).unwrap();
            c.execute("DELETE FROM industry_assignments", []).unwrap();
            c.execute("DELETE FROM industry_snapshots", []).unwrap();
        }

        fn rows(&self) -> (Vec<SnapshotRecord>, Vec<AssignmentRow>) {
            let c = rusqlite::Connection::open(&self.path).unwrap();
            (read_snapshots(&c).unwrap(), read_assignments(&c).unwrap())
        }

        fn summary(&self) -> String {
            let (snapshots, assignments) = self.rows();
            industry::summary(&snapshots, &assignments)
        }
    }

    /// Register one generation's members in the store and return the
    /// observation the daily item would commit.
    fn generation(
        store: &SourceStore,
        manifest: &str,
        cfg_bytes: &[u8],
        last_modified: Option<&str>,
        observed: &str,
    ) -> crate::industry::GenerationSnapshot {
        crate::industry::register_generation_members(
            store,
            &GenerationMembers {
                manifest_sha256: manifest,
                tdxhy_member: "tdxhy.cfg",
                tdxhy_bytes: cfg_bytes,
                tdxhy_publisher_last_modified: last_modified,
                tdxhy_observed_at: observed,
                incon_member: "zhb.zip!incon.dat",
                incon_bytes: DICTIONARY,
                incon_publisher_last_modified: last_modified,
                incon_observed_at: observed,
                base_zip_sha256: &"e".repeat(64),
            },
        )
        .unwrap()
    }

    /// The daily item's write: the observation plus the link the source store
    /// just told us.
    fn commit(
        backend: &MarketBackend,
        snapshot: &crate::industry::GenerationSnapshot,
    ) -> IndustryCommit {
        backend
            .commit_industry_snapshot(&IndustrySnapshotWrite {
                facts: &snapshot.facts,
                previous_member_sha256: snapshot.previous_member_sha256.as_deref(),
            })
            .unwrap()
    }

    #[test]
    fn daily_appends_then_a_fresh_init_replay_reproduces_the_same_table() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let v1 = cfg(&[
            (0, "000001", "T1001", "X500102"),
            (1, "600006", "T1001", "X500102"),
        ]);
        let v2 = cfg(&[
            (0, "000001", "T1001", "X500202"),
            (1, "600006", "T1001", "X500102"),
            (0, "000003", "T1001", "X500202"),
        ]);
        let v3 = cfg(&[
            (1, "600006", "T1001", "X500102"),
            (0, "000003", "T1001", "X500202"),
        ]);
        let manifests = ["1", "2", "3"];
        let inputs = [
            (
                v1.as_slice(),
                "Wed, 30 Sep 2026 08:00:00 GMT",
                "2026-09-30T09:00:00Z",
            ),
            (
                v2.as_slice(),
                "Mon, 12 Oct 2026 08:00:00 GMT",
                "2026-10-12T09:00:00Z",
            ),
            (
                v3.as_slice(),
                "Fri, 16 Oct 2026 08:00:00 GMT",
                "2026-10-16T09:00:00Z",
            ),
        ];

        // The daily path: register bytes first (§9.4.1), then extend the table.
        let backend = fixture.backend();
        let mut first_commit = None;
        for ((bytes, last_modified, observed), manifest) in inputs.iter().zip(manifests.iter()) {
            let facts = generation(
                &store,
                &format!("{}{manifest}", "f".repeat(63)),
                bytes,
                Some(*last_modified),
                observed,
            );
            let written = commit(&backend, &facts);
            assert!(!written.no_change);
            first_commit = Some(written);
        }
        let last_write = first_commit.unwrap();
        assert_eq!(last_write.snapshot_rows, 3);
        // v1: 2 rows; v2 closes 000001 at 10-12 and opens a new one (+1 net)
        // and adds 000003 (+1) → 4; v3 closes 000001 at 10-16 (600006 and
        // 000003 are unchanged) → still 4.
        assert_eq!(last_write.assignment_rows, 4);
        let daily_summary = fixture.summary();
        let (snapshots, assignments) = fixture.rows();
        assert_eq!(snapshots.len(), 3);
        assert_eq!(assignments.len(), 4);
        drop(backend);

        // Replaying the same snapshot again changes nothing (`no_change`).
        let backend = fixture.backend();
        let facts = generation(
            &store,
            &format!("{}3", "f".repeat(63)),
            v3.as_slice(),
            Some("Fri, 16 Oct 2026 08:00:00 GMT"),
            "2026-10-16T09:00:00Z",
        );
        let again = commit(&backend, &facts);
        assert!(again.no_change, "{again:?}");
        assert_eq!(fixture.summary(), daily_summary);
        drop(backend);

        // A fresh-init deployment loses the M3 tables; the source store is the
        // only durable copy, so `replay --scope industry` must rebuild exactly
        // the table the daily path maintained.
        fixture.clear_industry();
        let request = crate::source_store::replay::ReplayRequest::new(
            Some(crate::source_store::replay::Scope::Industry),
            false,
            Some(fixture.path.as_path()),
        );
        let report = crate::source_store::replay::replay(&store, &request).unwrap();
        assert_eq!(report.status, "ok", "{report:?}");
        let scope = &report.scopes[0];
        assert_eq!(scope.rebuilt, crate::source_store::replay::REBUILT_INDUSTRY);
        assert_eq!(scope.database, Some("match"));
        assert_eq!(scope.summary.as_deref(), Some(daily_summary.as_str()));
        assert_eq!(scope.boundaries, Some(3));
        assert_eq!(fixture.summary(), daily_summary);

        // A second replay is idempotent and `--verify-only` agrees.
        let report = crate::source_store::replay::replay(&store, &request).unwrap();
        assert_eq!(
            report.scopes[0].summary.as_deref(),
            Some(daily_summary.as_str())
        );
        let verify = crate::source_store::replay::ReplayRequest::new(
            Some(crate::source_store::replay::Scope::Industry),
            true,
            Some(fixture.path.as_path()),
        );
        let report = crate::source_store::replay::verify(&store, &verify).unwrap();
        assert_eq!(report.status, "ok", "{report:?}");
        assert_eq!(report.scopes[0].database, Some("match"));

        // And without `--db` the scope only computes: no table is touched.
        fixture.clear_industry();
        let compute_only = crate::source_store::replay::ReplayRequest::new(
            Some(crate::source_store::replay::Scope::Industry),
            false,
            None,
        );
        let report = crate::source_store::replay::replay(&store, &compute_only).unwrap();
        assert_eq!(
            report.scopes[0].rebuilt,
            crate::source_store::replay::COMPUTED_INDUSTRY_NO_DB
        );
        assert_eq!(
            report.scopes[0].summary.as_deref(),
            Some(daily_summary.as_str())
        );
        assert!(
            fixture.rows().0.is_empty(),
            "a read must not write the database"
        );
    }

    #[test]
    fn a_copy_without_a_publisher_time_never_changes_the_intervals() {
        // The archive shape at the table level: the same bytes registered again
        // without a publisher time (an old copy, fetched earlier but imported
        // later) is not an observation. It may only refine the stored row; the
        // interval and its `known_at` stay where the publisher time put them.
        let fixture = Fixture::new();
        let store = fixture.store();
        let bytes = cfg(&[(0, "000001", "T1001", "X500102")]);
        let backend = fixture.backend();
        let first = generation(
            &store,
            &format!("{}1", "d".repeat(63)),
            &bytes,
            Some("Mon, 12 Oct 2026 00:00:00 GMT"),
            "2026-10-12T09:00:00Z",
        );
        assert!(!commit(&backend, &first).no_change);
        let (snapshots, assignments) = fixture.rows();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(assignments[0].basis, "publisher_last_modified");
        assert_eq!(assignments[0].known_at, "2026-10-12T09:00:00Z");
        drop(backend);

        // The copy: same bytes, fetched on 10-08 but imported only on 10-13.
        let mut copy = Registration::new(
            crate::source_store::TDXHY_SOURCE_ID,
            StoreKind::ArchiveImport,
            "batch-9:2026-10-08/base.zip!tdxhy.cfg",
            "2026-10-08T01:00:00Z",
        );
        copy.evidence_grade = Some("B".to_string());
        copy.parser_version = Some("tdxhy-v1".to_string());
        copy.import_batch = Some("batch-9".to_string());
        copy.imported_at = Some("2026-10-13T00:00:00Z".to_string());
        store.register(&bytes, &copy).unwrap();

        // The store now holds evidence the table does not: `verify` says so
        // instead of silently agreeing.
        let verify_request = crate::source_store::replay::ReplayRequest::new(
            Some(crate::source_store::replay::Scope::Industry),
            true,
            Some(fixture.path.as_path()),
        );
        let drift = crate::source_store::replay::replay(&store, &verify_request).unwrap();
        assert_eq!(drift.status, "drift", "{drift:?}");
        assert_eq!(drift.scopes[0].database, Some("drift"));

        // A replay merges the copy into the stored row and leaves the intervals
        // exactly where the publisher time put them.
        let request = crate::source_store::replay::ReplayRequest::new(
            Some(crate::source_store::replay::Scope::Industry),
            false,
            Some(fixture.path.as_path()),
        );
        let report = crate::source_store::replay::replay(&store, &request).unwrap();
        assert_eq!(report.status, "ok", "{report:?}");
        assert_eq!(report.scopes[0].database, Some("match"));
        let (snapshots, assignments) = fixture.rows();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(
            snapshots[0].observed_at.as_deref(),
            Some("2026-10-08T01:00:00Z"),
            "the earliest fetch of the same bytes refines the row"
        );
        assert_eq!(
            snapshots[0].publisher_last_modified.as_deref(),
            Some("Mon, 12 Oct 2026 00:00:00 GMT")
        );
        assert_eq!(snapshots[0].evidence_grade, "A");
        assert_eq!(assignments.len(), 1);
        assert_eq!(assignments[0].basis, "publisher_last_modified");
        assert_eq!(
            assignments[0].known_at, "2026-10-12T09:00:00Z",
            "a copy without a publisher time dates nothing"
        );
    }

    #[test]
    fn a_content_that_comes_back_writes_a_third_interval_and_replays_the_same() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let a = cfg(&[
            (0, "000001", "T1001", "X500102"),
            (1, "600006", "T1001", "X500102"),
        ]);
        let b = cfg(&[
            (0, "000001", "T1001", "X500202"),
            (1, "600006", "T1001", "X500102"),
        ]);
        let observations = [
            (
                a.as_slice(),
                "1",
                "Wed, 30 Sep 2026 08:00:00 GMT",
                "2026-09-30T09:00:00Z",
            ),
            (
                b.as_slice(),
                "2",
                "Mon, 12 Oct 2026 08:00:00 GMT",
                "2026-10-12T09:00:00Z",
            ),
            // The publisher reverted the file: the first observation's bytes,
            // seen again two weeks later. It is a new observation, not a
            // duplicate of the stored snapshot.
            (
                a.as_slice(),
                "3",
                "Fri, 16 Oct 2026 08:00:00 GMT",
                "2026-10-16T09:00:00Z",
            ),
        ];
        let backend = fixture.backend();
        for (bytes, manifest, last_modified, observed) in observations {
            let snapshot = generation(
                &store,
                &format!("{}{manifest}", "a".repeat(63)),
                bytes,
                Some(last_modified),
                observed,
            );
            let written = commit(&backend, &snapshot);
            assert!(!written.no_change, "{written:?}");
        }
        drop(backend);

        let daily = fixture.summary();
        let (snapshots, assignments) = fixture.rows();
        assert_eq!(
            snapshots.len(),
            2,
            "storage keeps one row per content: A, B"
        );
        let reclassified: Vec<&AssignmentRow> = assignments
            .iter()
            .filter(|row| row.code == "000001")
            .collect();
        assert_eq!(reclassified.len(), 3, "A → B → A is two changes");
        assert_eq!(reclassified[0].industry_code, "X5001");
        assert_eq!(reclassified[1].industry_code, "X5002");
        assert_eq!(reclassified[2].industry_code, "X5001");
        assert_eq!(reclassified[2].effective_to, None);
        // The code that never changed keeps one interval across all three.
        assert_eq!(
            assignments
                .iter()
                .filter(|row| row.code == "600006")
                .count(),
            1
        );

        // The source store is the only durable copy: replaying it must produce
        // the same three intervals, not the two a content-keyed dedupe leaves.
        fixture.clear_industry();
        let request = crate::source_store::replay::ReplayRequest::new(
            Some(crate::source_store::replay::Scope::Industry),
            false,
            Some(fixture.path.as_path()),
        );
        let report = crate::source_store::replay::replay(&store, &request).unwrap();
        assert_eq!(report.status, "ok", "{report:?}");
        let scope = &report.scopes[0];
        assert_eq!(scope.database, Some("match"));
        assert_eq!(scope.summary.as_deref(), Some(daily.as_str()));
        assert_eq!(scope.boundaries, Some(3));
        assert_eq!(fixture.summary(), daily);
    }

    #[test]
    fn a_backfilled_archive_snapshot_gets_the_import_time_as_known_at() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let bytes = cfg(&[(0, "000001", "T1001", "X500102")]);
        let mut facts = Registration::new(
            crate::source_store::TDXHY_SOURCE_ID,
            StoreKind::ArchiveImport,
            "batch-1:2026-09-30/base.zip!tdxhy.cfg",
            "2026-09-30T08:00:00Z",
        );
        facts.publisher_last_modified = Some("Wed, 30 Sep 2026 08:00:00 GMT".to_string());
        facts.evidence_grade = Some("A".to_string());
        facts.parser_version = Some("tdxhy-v1".to_string());
        facts.import_batch = Some("batch-1".to_string());
        facts.imported_at = Some("2026-10-28T00:00:00Z".to_string());
        store.register(&bytes, &facts).unwrap();

        let request = crate::source_store::replay::ReplayRequest::new(
            Some(crate::source_store::replay::Scope::Industry),
            false,
            Some(fixture.path.as_path()),
        );
        let report = crate::source_store::replay::replay(&store, &request).unwrap();
        assert_eq!(report.status, "ok", "{report:?}");
        let (snapshots, assignments) = fixture.rows();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].source_kind, SourceKind::Archive);
        assert_eq!(snapshots[0].evidence_grade, "A");
        assert_eq!(assignments.len(), 1);
        // The interval starts at the publisher's date but is known only from
        // the import: back-dating it would claim knowledge we did not have.
        assert_eq!(
            assignments[0].effective_from,
            crate::industry::epoch_day(chrono::NaiveDate::from_ymd_opt(2026, 9, 30).unwrap())
        );
        assert_eq!(assignments[0].known_at, "2026-10-28T00:00:00Z");
        assert_eq!(assignments[0].name_status, "unknown_dictionary");
        // The archive import has no generation manifest, and the dictionary of
        // the same batch is not registered, so no snapshot claims one.
        assert!(snapshots[0].generation_manifest_sha256.is_none());
        assert!(snapshots[0].dictionary_sha256.is_none());

        // Idempotent: the same import + replay adds nothing.
        let summary = fixture.summary();
        let report = crate::source_store::replay::replay(&store, &request).unwrap();
        assert_eq!(report.status, "ok");
        assert_eq!(fixture.summary(), summary);
        assert_eq!(fixture.rows().1.len(), 1);
    }

    #[test]
    fn a_missing_blob_fails_the_scope_as_source_store_corrupt() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let backend = fixture.backend();
        let facts = generation(
            &store,
            &"a".repeat(64),
            &cfg(&[(0, "000001", "T1001", "X500102")]),
            Some("Wed, 30 Sep 2026 08:00:00 GMT"),
            "2026-09-30T09:00:00Z",
        );
        commit(&backend, &facts);
        drop(backend);
        // Drop the blob: the store no longer proves the content its manifest
        // still names, so nothing may be rebuilt from it (§9.4.1).
        std::fs::remove_file(store.blob_path(&facts.facts.record.member_sha256)).unwrap();

        let request = crate::source_store::replay::ReplayRequest::new(
            Some(crate::source_store::replay::Scope::Industry),
            true,
            Some(fixture.path.as_path()),
        );
        let report = crate::source_store::replay::verify(&store, &request).unwrap();
        assert_eq!(report.status, "source_store_corrupt", "{report:?}");
    }

    #[test]
    fn replay_refuses_an_index_row_the_store_does_not_hold() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let backend = fixture.backend();
        let facts = generation(
            &store,
            &"c".repeat(64),
            &cfg(&[(0, "000001", "T1001", "X500102")]),
            Some("Wed, 30 Sep 2026 08:00:00 GMT"),
            "2026-09-30T09:00:00Z",
        );
        commit(&backend, &facts);
        drop(backend);
        // An index row the store cannot prove: the store is authoritative, so a
        // replay refuses instead of silently dropping the row (§9.4.1).
        let c = rusqlite::Connection::open(&fixture.path).unwrap();
        c.execute(
            "INSERT INTO industry_snapshots(member_sha256,store_sha256,source_kind,ingested_at,\
             evidence_grade,parser_version,row_count,scope_member_count) \
             VALUES(?1,?2,'repo_evidence','2026-10-01T00:00:00Z','C','tdxhy-v1',1,0)",
            rusqlite::params![vec![0xcdu8; 32], vec![0xcdu8; 32]],
        )
        .unwrap();
        drop(c);

        let request = crate::source_store::replay::ReplayRequest::new(
            Some(crate::source_store::replay::Scope::Industry),
            false,
            Some(fixture.path.as_path()),
        );
        let report = crate::source_store::replay::replay(&store, &request).unwrap();
        assert_eq!(report.status, "industry_rebuild_failed", "{report:?}");
        let problem = &report.problems[0];
        assert_eq!(problem.reason, "industry_rebuild_failed");
        assert!(
            problem.actual.contains("source_store_index_orphan"),
            "{}",
            problem.actual
        );
        // Nothing was deleted before the refusal: the row is still there.
        assert_eq!(fixture.rows().0.len(), 2);
    }

    /// The rebuild's snapshot list is exactly what the store holds, so a second
    /// play cannot grow the table.
    #[test]
    fn replay_rebuild_counts_match_the_store() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let facts = generation(
            &store,
            &"b".repeat(64),
            &cfg(&[(0, "000001", "T1001", "X500102")]),
            None,
            "2026-09-30T09:00:00Z",
        );
        let backend = fixture.backend();
        commit(&backend, &facts);
        drop(backend);
        let rebuild: Rebuild =
            industry::rebuild(&crate::sqlite_market::industry::facts_from_store(&store).unwrap())
                .unwrap();
        assert_eq!(rebuild.snapshots.len(), 1);
        assert_eq!(rebuild.assignments.len(), 1);
        assert_eq!(
            rebuild.snapshots[0].evidence_grade, "B",
            "no publisher time means grade B, and the start is the acquisition day"
        );
        assert_eq!(rebuild.assignments[0].basis, "acquisition_time");
    }
}
