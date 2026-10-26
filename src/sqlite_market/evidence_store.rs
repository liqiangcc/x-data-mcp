//! Kept evidence machinery for the schema-21 latest-only store
//! (S2: retired from the version-chain modules without changing wire
//! semantics). Evidence bundles and source bindings remain
//! content-addressed rows; the only change is that versions no longer
//! reference segment chains — market events and series rows carry the
//! digests directly.
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

use super::{integrity, storage};
use crate::compact_market::{self, Digest, EvidenceKind, SourceBinding};
use crate::market_storage::Error;

const SHARED_VERSION_FIELDS: [&str; 2] = ["unit_currency", "adjustment_evidence"];
const EVREF: &str = "$evref";

pub(crate) fn put_evidence(
    c: &Connection,
    kind: EvidenceKind,
    value: &Value,
) -> Result<Digest, Error> {
    let bundle = compact_market::canonical_evidence(kind, value).map_err(integrity)?;
    let existing = c
        .query_row(
            "SELECT evidence_kind,canonical_json FROM evidence_bundles WHERE evidence_digest=?1",
            params![bundle.digest.as_slice()],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(storage)?;
    if let Some((kind_raw, stored)) = existing {
        let stored_kind = parse_evidence_kind(&kind_raw)?;
        let canonical = inflate_evidence(c, stored_kind, &stored)?;
        compact_market::verify_evidence(stored_kind, &canonical, bundle.digest)
            .map_err(integrity)?;
        if stored_kind != kind || canonical != bundle.canonical_json {
            return Err(integrity(
                "evidence digest collision with different content",
            ));
        }
        return Ok(bundle.digest);
    }
    let stored_form = decompose_evidence(c, kind, &bundle.canonical_json)?;
    c.execute(
        "INSERT INTO evidence_bundles(evidence_digest,evidence_kind,canonical_json) VALUES(?1,?2,?3)",
        params![bundle.digest.as_slice(), kind.as_str(), stored_form],
    )
    .map_err(storage)?;
    Ok(bundle.digest)
}

pub(crate) fn read_evidence(
    c: &Connection,
    digest: &Digest,
) -> Result<(EvidenceKind, String), Error> {
    let row: Option<(String, String)> = c
        .query_row(
            "SELECT evidence_kind,canonical_json FROM evidence_bundles WHERE evidence_digest=?1",
            params![digest.as_slice()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(storage)?;
    let Some((kind_raw, stored)) = row else {
        return Err(integrity("evidence bundle missing"));
    };
    let kind = parse_evidence_kind(&kind_raw)?;
    let canonical = inflate_evidence(c, kind, &stored)?;
    compact_market::verify_evidence(kind, &canonical, *digest).map_err(integrity)?;
    Ok((kind, canonical))
}

fn parse_evidence_kind(raw: &str) -> Result<EvidenceKind, Error> {
    Ok(match raw {
        "generation" => EvidenceKind::Generation,
        "batch_selection" => EvidenceKind::BatchSelection,
        "version" => EvidenceKind::Version,
        "continuity" => EvidenceKind::Continuity,
        "catalog" => EvidenceKind::Catalog,
        "yearly" => EvidenceKind::Yearly,
        "derived" => EvidenceKind::Derived,
        "shared" => EvidenceKind::Shared,
        _ => return Err(integrity("unknown evidence kind")),
    })
}

/// Replace decomposable fields with `$evref` markers pointing at `shared`
/// sub-bundle rows. Returns the stored form (full canonical unchanged for
/// kinds without decomposable fields).
fn decompose_evidence(
    c: &Connection,
    kind: EvidenceKind,
    canonical: &str,
) -> Result<String, Error> {
    if kind != EvidenceKind::Version {
        return Ok(canonical.to_string());
    }
    let mut doc: Value = serde_json::from_str(canonical).map_err(integrity)?;
    let Some(map) = doc.as_object_mut() else {
        return Err(integrity("version evidence is not an object"));
    };
    for field in SHARED_VERSION_FIELDS {
        let Some(existing) = map.get(field) else {
            continue;
        };
        if !existing.is_object() || existing.get(EVREF).is_some() {
            continue;
        }
        let sub = compact_market::canonical_evidence(EvidenceKind::Shared, existing)
            .map_err(integrity)?;
        store_shared(c, &sub)?;
        map.insert(
            field.to_string(),
            json!({EVREF: compact_market::digest_hex(&sub.digest)}),
        );
    }
    serde_json::to_string(&doc).map_err(integrity)
}

/// Insert a `shared` sub-document row verbatim, or verify an identical row
/// already stored under its digest. Part rows live in `evidence_parts` and
/// are never decomposed, so marker expansion cannot recurse.
fn store_shared(c: &Connection, bundle: &compact_market::EvidenceBundle) -> Result<(), Error> {
    let existing = c
        .query_row(
            "SELECT canonical_json FROM evidence_parts WHERE part_digest=?1",
            params![bundle.digest.as_slice()],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(storage)?;
    if let Some(canonical) = existing {
        compact_market::verify_evidence(EvidenceKind::Shared, &canonical, bundle.digest)
            .map_err(integrity)?;
        if canonical != bundle.canonical_json {
            return Err(integrity(
                "evidence digest collision with different content",
            ));
        }
        return Ok(());
    }
    c.execute(
        "INSERT INTO evidence_parts(part_digest,canonical_json) VALUES(?1,?2)",
        params![bundle.digest.as_slice(), bundle.canonical_json],
    )
    .map_err(storage)?;
    Ok(())
}

/// Re-inline `$evref` markers back into a stored evidence document and
/// return its canonical form. Kinds without decomposable fields — and rows
/// written before decomposition existed — pass through unchanged.
fn inflate_evidence(c: &Connection, kind: EvidenceKind, stored: &str) -> Result<String, Error> {
    if kind != EvidenceKind::Version {
        return Ok(stored.to_string());
    }
    let mut doc: Value = serde_json::from_str(stored).map_err(integrity)?;
    let Some(map) = doc.as_object_mut() else {
        return Err(integrity("stored version evidence is not an object"));
    };
    for field in SHARED_VERSION_FIELDS {
        let Some(marker) = map
            .get(field)
            .and_then(|v| v.get(EVREF))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let digest = digest_from_hex(marker)?;
        let canonical: String = c
            .query_row(
                "SELECT canonical_json FROM evidence_parts WHERE part_digest=?1",
                params![digest.as_slice()],
                |r| r.get(0),
            )
            .optional()
            .map_err(storage)?
            .ok_or_else(|| integrity("shared evidence part missing"))?;
        compact_market::verify_evidence(EvidenceKind::Shared, &canonical, digest)
            .map_err(integrity)?;
        map.insert(
            field.to_string(),
            serde_json::from_str(&canonical).map_err(integrity)?,
        );
    }
    serde_json_canonicalizer::to_string(&doc).map_err(|_| integrity("evidence not canonicalizable"))
}

fn digest_from_hex(hex: &str) -> Result<Digest, Error> {
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16))
        .collect::<Result<Vec<u8>, _>>()
        .map_err(|_| integrity("malformed shared evidence reference"))?;
    bytes
        .try_into()
        .map_err(|_| integrity("malformed shared evidence reference"))
}

pub(crate) fn put_source_binding(c: &Connection, binding: &SourceBinding) -> Result<Digest, Error> {
    let digest = compact_market::source_binding_digest(binding).map_err(integrity)?;
    let (kind, _) = read_evidence(c, &binding.evidence_digest)?;
    if kind != EvidenceKind::Generation {
        return Err(integrity("source binding evidence is not generation"));
    }
    let existing = c
        .query_row(
            "SELECT generation_id,generation_manifest_sha256,day_member_sha256,security_event_sha256,evidence_digest FROM source_bindings WHERE source_binding_digest=?1",
            params![digest.as_slice()],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Vec<u8>>(1)?,
                    r.get::<_, Vec<u8>>(2)?,
                    r.get::<_, Vec<u8>>(3)?,
                    r.get::<_, Vec<u8>>(4)?,
                ))
            },
        )
        .optional()
        .map_err(storage)?;
    if let Some((generation_id, manifest, day, event, evidence)) = existing {
        let stored = SourceBinding {
            generation_id,
            generation_manifest_sha256: fixed_digest(&manifest)?,
            day_member_sha256: fixed_digest(&day)?,
            security_event_sha256: fixed_digest(&event)?,
            evidence_digest: fixed_digest(&evidence)?,
        };
        if compact_market::source_binding_digest(&stored).map_err(integrity)? != digest {
            return Err(integrity("stored source binding digest mismatch"));
        }
        if &stored != binding {
            return Err(integrity(
                "source binding digest collision with different content",
            ));
        }
        return Ok(digest);
    }
    c.execute(
        "INSERT INTO source_bindings(source_binding_digest,generation_id,generation_manifest_sha256,day_member_sha256,security_event_sha256,evidence_digest) VALUES(?1,?2,?3,?4,?5,?6)",
        params![
            digest.as_slice(),
            binding.generation_id,
            binding.generation_manifest_sha256.as_slice(),
            binding.day_member_sha256.as_slice(),
            binding.security_event_sha256.as_slice(),
            binding.evidence_digest.as_slice()
        ],
    )
    .map_err(storage)?;
    Ok(digest)
}

/// `source_bindings` row as stored.
type BindingRow = (String, Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>);

pub(crate) fn read_source_binding(c: &Connection, digest: &Digest) -> Result<SourceBinding, Error> {
    let row: Option<BindingRow> = c
        .query_row(
            "SELECT generation_id,generation_manifest_sha256,day_member_sha256,security_event_sha256,evidence_digest FROM source_bindings WHERE source_binding_digest=?1",
            params![digest.as_slice()],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                ))
            },
        )
        .optional()
        .map_err(storage)?;
    let Some(row) = row else {
        return Err(integrity("source binding missing"));
    };
    let binding = SourceBinding {
        generation_id: row.0,
        generation_manifest_sha256: fixed_digest(&row.1)?,
        day_member_sha256: fixed_digest(&row.2)?,
        security_event_sha256: fixed_digest(&row.3)?,
        evidence_digest: fixed_digest(&row.4)?,
    };
    if compact_market::source_binding_digest(&binding).map_err(integrity)? != *digest {
        return Err(integrity("source binding digest mismatch"));
    }
    let (kind, _) = read_evidence(c, &binding.evidence_digest)?;
    if kind != EvidenceKind::Generation {
        return Err(integrity("source binding evidence is not generation"));
    }
    Ok(binding)
}

fn fixed_digest(raw: &[u8]) -> Result<Digest, Error> {
    raw.try_into()
        .map_err(|_| integrity("stored digest length mismatch"))
}

/// Verified presence check used at commit gates: the binding row exists,
/// its digest recomputes, and its generation evidence kind-checks
/// (`read_source_binding` already performs all three).
pub(crate) fn verified_source_binding(c: &Connection, digest: &Digest) -> Result<(), Error> {
    read_source_binding(c, digest).map(|_| ())
}
