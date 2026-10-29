//! Storage mapping for the M3 retirement-gate tables (design `#446`
//! §8.2/§8.3/§9.4.2/§9.4.3).
//!
//! Two tables, two very different trust levels:
//!
//! - `generation_facts` is a **rebuildable index** of the derived facts a
//!   generation produced. The writer (industry item, market daily run)
//!   registers `(role, fact_kind, fact_digest, evidence_sha256)`; the replay
//!   (`--scope generation_facts`) recomputes the same rows from the source
//!   store (industry) and from the derived tables' generation bindings
//!   (capital/adjustment events, daily series). The retire gate treats a
//!   missing row as "replay first", never as proof that the facts are gone.
//! - `retire_exemptions` is the explicit exception path: a non-blank reason,
//!   who recorded it and when. It is never written by the retire path itself,
//!   only by the operator command (`source_store exempt`).
//!
//! `fact_digest` is a content digest, not a timestamp-dependent value, so
//! "clear the DB and replay" reproduces the same summary (§9.4.2). Bytes for
//! the irreproducible facts stay in the source store; for the facts that the
//! derived tables already explain, the digest is computed from those tables.

use std::path::Path;

use rusqlite::{Connection, params};

use super::{integrity, storage};
use crate::generation_gate::{
    GenerationFact, KIND_ADJUSTMENT_EVENTS, KIND_CAPITAL_EVENTS, KIND_DAILY_SERIES, ROLE_DAY,
    ROLE_GBBQ, RetireExemption, timestamp,
};
use crate::hash::sha256;
use crate::market_storage::Error;
use crate::sqlite_tasks::{COMPACT_SCHEMA_VERSION, SqliteTasks};

/// Encode a validated hex digest into the 32-byte BLOB every index uses.
fn blob(value: &str) -> Result<Vec<u8>, Error> {
    crate::source_store::validate_sha256(value)
        .map_err(|error| integrity(format!("{value:?}: {error}")))?;
    let bytes = (0..value.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&value[index..index + 2], 16))
        .collect::<Result<Vec<u8>, _>>()
        .map_err(|_| integrity(format!("{value:?} is not hex")))?;
    Ok(bytes)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn optional_blob(value: Option<&str>) -> Result<Option<Vec<u8>>, Error> {
    value.map(blob).transpose()
}

fn optional_hex(value: Option<Vec<u8>>) -> Option<String> {
    value.map(|bytes| hex(&bytes))
}

/// Upsert the facts a successful writer registered. Re-registering the same
/// `(generation, role, kind)` refreshes the digest and the registration time,
/// so a rerun cannot leave a stale digest behind.
pub(crate) fn record_facts(c: &Connection, facts: &[GenerationFact]) -> Result<(), Error> {
    let mut stmt = c
        .prepare(
            "INSERT INTO generation_facts(generation_manifest_sha256,role,fact_kind,\
             fact_digest,evidence_sha256,parser_version,registered_at) \
             VALUES(?1,?2,?3,?4,?5,?6,?7) \
             ON CONFLICT(generation_manifest_sha256,role,fact_kind) DO UPDATE SET \
             fact_digest=excluded.fact_digest,evidence_sha256=excluded.evidence_sha256,\
             parser_version=excluded.parser_version,registered_at=excluded.registered_at",
        )
        .map_err(storage)?;
    for fact in facts {
        stmt.execute(params![
            blob(&fact.generation_manifest_sha256)?,
            fact.role,
            fact.fact_kind,
            blob(&fact.fact_digest)?,
            optional_blob(fact.evidence_sha256.as_deref())?,
            fact.parser_version,
            fact.registered_at,
        ])
        .map_err(storage)?;
    }
    Ok(())
}

const FACT_COLUMNS: &str = "generation_manifest_sha256,role,fact_kind,fact_digest,\
evidence_sha256,parser_version,registered_at";

fn map_fact(row: &rusqlite::Row<'_>) -> Result<GenerationFact, Error> {
    let generation: Vec<u8> = row.get(0).map_err(storage)?;
    let digest: Vec<u8> = row.get(3).map_err(storage)?;
    let evidence: Option<Vec<u8>> = row.get(4).map_err(storage)?;
    Ok(GenerationFact {
        generation_manifest_sha256: hex(&generation),
        role: row.get(1).map_err(storage)?,
        fact_kind: row.get(2).map_err(storage)?,
        fact_digest: hex(&digest),
        evidence_sha256: optional_hex(evidence),
        parser_version: row.get(5).map_err(storage)?,
        registered_at: row.get(6).map_err(storage)?,
    })
}

/// Every registered fact, or only one generation's rows.
pub(crate) fn read_facts(
    c: &Connection,
    generation: Option<&str>,
) -> Result<Vec<GenerationFact>, Error> {
    match generation {
        Some(generation) => {
            let mut stmt = c
                .prepare(&format!(
                    "SELECT {FACT_COLUMNS} FROM generation_facts \
                     WHERE generation_manifest_sha256=?1 ORDER BY role,fact_kind"
                ))
                .map_err(storage)?;
            let rows = stmt
                .query_map(params![blob(generation)?], map_fact)
                .map_err(storage)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(storage)
        }
        None => {
            let mut stmt = c
                .prepare(&format!(
                    "SELECT {FACT_COLUMNS} FROM generation_facts \
                     ORDER BY generation_manifest_sha256,role,fact_kind"
                ))
                .map_err(storage)?;
            let rows = stmt.query_map([], map_fact).map_err(storage)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(storage)
        }
    }
}

/// Replace the whole table (replay's one transaction).
pub(crate) fn replace_facts(c: &Connection, facts: &[GenerationFact]) -> Result<(), Error> {
    c.execute("DELETE FROM generation_facts", [])
        .map_err(storage)?;
    record_facts(c, facts)
}

const EXEMPTION_COLUMNS: &str =
    "generation_manifest_sha256,missing_kind,reason,recorded_by,recorded_at";

fn map_exemption(row: &rusqlite::Row<'_>) -> Result<RetireExemption, Error> {
    let generation: Vec<u8> = row.get(0).map_err(storage)?;
    Ok(RetireExemption {
        generation_manifest_sha256: hex(&generation),
        missing_kind: row.get(1).map_err(storage)?,
        reason: row.get(2).map_err(storage)?,
        recorded_by: row.get(3).map_err(storage)?,
        recorded_at: row.get(4).map_err(storage)?,
    })
}

pub(crate) fn read_exemptions(
    c: &Connection,
    generation: &str,
) -> Result<Vec<RetireExemption>, Error> {
    let mut stmt = c
        .prepare(&format!(
            "SELECT {EXEMPTION_COLUMNS} FROM retire_exemptions \
             WHERE generation_manifest_sha256=?1 ORDER BY missing_kind"
        ))
        .map_err(storage)?;
    let rows = stmt
        .query_map(params![blob(generation)?], map_exemption)
        .map_err(storage)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(storage)
}

/// Insert one operator-recorded exemption. A blank reason or recorder is
/// refused here as well as by the CLI, so no path can write an anonymous
/// exemption.
pub(crate) fn record_exemption(c: &Connection, exemption: &RetireExemption) -> Result<(), Error> {
    if exemption.reason.trim().is_empty() {
        return Err(integrity("retire exemption requires a non-blank reason"));
    }
    if exemption.recorded_by.trim().is_empty() {
        return Err(integrity("retire exemption requires recorded_by"));
    }
    c.execute(
        "INSERT INTO retire_exemptions(generation_manifest_sha256,missing_kind,reason,\
         recorded_by,recorded_at) VALUES(?1,?2,?3,?4,?5) \
         ON CONFLICT(generation_manifest_sha256,missing_kind) DO UPDATE SET \
         reason=excluded.reason,recorded_by=excluded.recorded_by,\
         recorded_at=excluded.recorded_at",
        params![
            blob(&exemption.generation_manifest_sha256)?,
            exemption.missing_kind,
            exemption.reason,
            exemption.recorded_by,
            exemption.recorded_at,
        ],
    )
    .map_err(storage)?;
    Ok(())
}

/// Read the gate's rows through a read-only connection, the path the acquire
/// process uses. A missing/old-schema database is an error, never an empty
/// index: deleting bytes because the index "looked empty" is the failure this
/// gate exists to prevent.
pub(crate) fn read_state_readonly(
    database: &Path,
    generation: &str,
) -> Result<(Vec<GenerationFact>, Vec<RetireExemption>), Error> {
    let tasks = SqliteTasks::new(database);
    let mut connection = tasks.connect(true)?;
    let version: u32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(storage)?;
    if version != COMPACT_SCHEMA_VERSION {
        return Err(storage(format!(
            "generation facts require schema {COMPACT_SCHEMA_VERSION}, {database:?} is {version}"
        )));
    }
    let transaction = connection.transaction().map_err(storage)?;
    let facts = read_facts(&transaction, Some(generation))?;
    let exemptions = read_exemptions(&transaction, generation)?;
    transaction.commit().map_err(storage)?;
    Ok((facts, exemptions))
}

/// Recompute the market-derived fact rows for one generation from the
/// derived tables' `source_bindings` (§9.4.2).
///
/// Only kinds that actually have rows bound to the generation are returned:
/// a generation that wrote no capital events has no `capital_events` fact,
/// and the gate refuses it rather than the writer inventing an empty one.
pub(crate) fn derived_facts_for(
    c: &Connection,
    generation: &str,
) -> Result<Vec<GenerationFact>, Error> {
    let generation_blob = blob(generation)?;
    let now = timestamp(chrono::Utc::now());
    let mut rows = Vec::new();
    if let Some(digest) = daily_series_digest(c, &generation_blob)? {
        rows.push(GenerationFact {
            generation_manifest_sha256: generation.to_string(),
            role: ROLE_DAY.into(),
            fact_kind: KIND_DAILY_SERIES.into(),
            fact_digest: digest,
            evidence_sha256: None,
            parser_version: None,
            registered_at: now.clone(),
        });
    }
    if let Some(digest) = capital_events_digest(c, &generation_blob)? {
        rows.push(GenerationFact {
            generation_manifest_sha256: generation.to_string(),
            role: ROLE_GBBQ.into(),
            fact_kind: KIND_CAPITAL_EVENTS.into(),
            fact_digest: digest,
            evidence_sha256: None,
            parser_version: None,
            registered_at: now.clone(),
        });
    }
    if let Some(digest) = adjustment_events_digest(c, &generation_blob)? {
        rows.push(GenerationFact {
            generation_manifest_sha256: generation.to_string(),
            role: ROLE_GBBQ.into(),
            fact_kind: KIND_ADJUSTMENT_EVENTS.into(),
            fact_digest: digest,
            evidence_sha256: None,
            parser_version: None,
            registered_at: now,
        });
    }
    Ok(rows)
}

/// Recompute every generation's market-derived facts (replay's DB half).
pub(crate) fn all_derived_facts(c: &Connection) -> Result<Vec<GenerationFact>, Error> {
    let mut stmt = c
        .prepare("SELECT DISTINCT generation_manifest_sha256 FROM source_bindings")
        .map_err(storage)?;
    let generations = stmt
        .query_map([], |row| row.get::<_, Vec<u8>>(0))
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    let mut rows = Vec::new();
    for generation in generations {
        rows.extend(derived_facts_for(c, &hex(&generation))?);
    }
    Ok(rows)
}

fn content_digest(kind: &str, generation: &[u8], lines: &[String]) -> String {
    sha256(
        format!(
            "generation-fact|{kind}|{}|{}",
            hex(generation),
            lines.join("\n")
        )
        .as_bytes(),
    )
}

fn daily_series_digest(c: &Connection, generation: &[u8]) -> Result<Option<String>, Error> {
    let mut stmt = c
        .prepare(
            "SELECT m.entity,m.period,m.adjustment,m.version_id FROM market_series m \
             JOIN source_bindings b ON b.source_binding_digest=m.source_binding_digest \
             WHERE b.generation_manifest_sha256=?1 \
             ORDER BY m.entity,m.period,m.adjustment",
        )
        .map_err(storage)?;
    let lines = stmt
        .query_map(params![generation], |row| {
            Ok(format!(
                "{}|{}|{}|{}",
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    if lines.is_empty() {
        return Ok(None);
    }
    Ok(Some(content_digest(KIND_DAILY_SERIES, generation, &lines)))
}

fn capital_events_digest(c: &Connection, generation: &[u8]) -> Result<Option<String>, Error> {
    let mut stmt = c
        .prepare(
            "SELECT e.exchange,e.code,e.ordinal,e.effective_day,e.category,\
             e.total_after,e.float_after FROM capital_events e \
             JOIN source_bindings b ON b.source_binding_digest=e.source_binding_digest \
             WHERE b.generation_manifest_sha256=?1 \
             ORDER BY e.exchange,e.code,e.ordinal",
        )
        .map_err(storage)?;
    let lines = stmt
        .query_map(params![generation], |row| {
            Ok(format!(
                "{}|{}|{}|{}|{}|{}|{}",
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    if lines.is_empty() {
        return Ok(None);
    }
    Ok(Some(content_digest(
        KIND_CAPITAL_EVENTS,
        generation,
        &lines,
    )))
}

fn adjustment_events_digest(c: &Connection, generation: &[u8]) -> Result<Option<String>, Error> {
    let mut stmt = c
        .prepare(
            "SELECT a.entity,a.ordinal,a.effective_day,a.c1,a.c2,a.c3,a.c4 \
             FROM adjustment_events a \
             JOIN source_bindings b ON b.source_binding_digest=a.source_binding_digest \
             WHERE b.generation_manifest_sha256=?1 \
             ORDER BY a.entity,a.ordinal",
        )
        .map_err(storage)?;
    let lines = stmt
        .query_map(params![generation], |row| {
            Ok(format!(
                "{}|{}|{}|{}|{}|{}|{}",
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ))
        })
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    if lines.is_empty() {
        return Ok(None);
    }
    Ok(Some(content_digest(
        KIND_ADJUSTMENT_EVENTS,
        generation,
        &lines,
    )))
}
