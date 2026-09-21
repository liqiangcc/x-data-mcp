//! Compact version envelopes. `data_versions.envelope_json` stores the
//! compatibility record with bulk bar arrays replaced by `$ref` markers that
//! point at shared `manifest_nodes` segment clips or content-addressed `payloads`
//! rows. Expansion restores the exact record; `compat::check` then re-verifies
//! the legacy digest, so a broken reference fails closed.
use super::*;
use serde_json::Value;
const REF: &str = "$ref";
/// A content-addressed array payload staged for one insert.
#[derive(Debug, Clone)]
pub(crate) struct Payload {
    pub id: String,
    pub json: String,
}
fn ref_segments() -> Value {
    serde_json::json!({REF: "segments"})
}
fn ref_records() -> Value {
    serde_json::json!({REF: "records"})
}
pub(crate) fn ref_payload(id: &str) -> Value {
    serde_json::json!({REF: format!("payload:{id}")})
}
fn ref_annual_bars(id: &str) -> Value {
    serde_json::json!({REF: format!("annual_bars:{id}")})
}
pub(crate) fn array_payload(v: &Value) -> Result<Payload, Error> {
    if !v.is_array() {
        return Err(integrity("envelope payload is not an array"));
    }
    let json = text(v)?;
    Ok(Payload {
        id: crate::hash::sha256(json.as_bytes()),
        json,
    })
}
/// Daily pack: `records` is served from the manifest (`marker` selects
/// segment- vs payload-backed reconstruction for migrated rows whose bars
/// cannot be reproduced exactly from shared segments).
pub(crate) fn pack_daily(raw: &Value, marker: Value) -> Result<(Value, Vec<Payload>), Error> {
    let mut env = raw.clone();
    let mut out = Vec::new();
    env["records"] = marker;
    let bars = &raw["batch"]["bars"];
    env["batch"]["bars"] = if *bars == raw["records"] {
        ref_records()
    } else {
        let p = array_payload(bars)?;
        let id = p.id.clone();
        out.push(p);
        ref_payload(&id)
    };
    Ok((env, out))
}
/// Yearly pack: the `annual.records` derivation array is canonical when the
/// record carries it; `records`/`batch.bars` reduce to `.bar` projections of
/// it when equal, or to their own payloads otherwise.
fn pack_yearly(raw: &Value) -> Result<(Value, Vec<Payload>), Error> {
    let mut env = raw.clone();
    let mut out = Vec::new();
    let mut annual_bars = Value::Null;
    if let Some(annual) = raw
        .pointer("/local_yearly_derivation_v1/annual/records")
        .filter(|v| v.is_array())
        .cloned()
    {
        let p = array_payload(&annual)?;
        *env.pointer_mut("/local_yearly_derivation_v1/annual/records")
            .unwrap() = ref_payload(&p.id);
        annual_bars = Value::Array(
            annual
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["bar"].clone())
                .collect(),
        );
        out.push(p);
        let records = &raw["records"];
        env["records"] = if *records == annual_bars {
            ref_annual_bars(out.last().unwrap().id.clone().as_str())
        } else {
            let p = array_payload(records)?;
            let id = p.id.clone();
            out.push(p);
            ref_payload(&id)
        };
    } else {
        let p = array_payload(&raw["records"])?;
        let id = p.id.clone();
        out.push(p);
        env["records"] = ref_payload(&id);
    }
    let bars = &raw["batch"]["bars"];
    env["batch"]["bars"] = if *bars == raw["records"] {
        ref_records()
    } else if *bars == annual_bars {
        ref_annual_bars(&out[0].id)
    } else {
        let p = array_payload(bars)?;
        let id = p.id.clone();
        out.push(p);
        ref_payload(&id)
    };
    Ok((env, out))
}
/// Strip bulk arrays from a version/audit record into `payloads` rows.
/// `records` for daily versions is served from the manifest instead.
pub(crate) fn pack(raw: &Value) -> Result<(Value, Vec<Payload>), Error> {
    if raw["period"].as_str() == Some("Day") {
        pack_daily(raw, ref_segments())
    } else {
        pack_yearly(raw)
    }
}
/// Replace `bars` inside a serialized `MarketDataBatch` with a payload
/// reference; used for audit segment evidence.
pub(crate) fn pack_batch(batch: &Value) -> Result<(Value, Vec<Payload>), Error> {
    let mut out = batch.clone();
    let p = array_payload(&batch["bars"])?;
    out["bars"] = ref_payload(&p.id);
    Ok((out, vec![p]))
}
pub(crate) fn write_payloads(c: &Connection, payloads: &[Payload]) -> Result<(), Error> {
    let mut stmt = c
        .prepare("INSERT OR IGNORE INTO payloads VALUES(?1,?2)")
        .map_err(storage)?;
    for p in payloads {
        stmt.execute(params![p.id, p.json]).map_err(storage)?;
    }
    Ok(())
}
fn fetch_payload(c: &Connection, id: &str) -> Result<Value, Error> {
    let json: Option<String> = c
        .query_row(
            "SELECT payload_json FROM payloads WHERE payload_id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()
        .map_err(storage)?;
    let json = json.ok_or_else(|| integrity("referenced payload missing"))?;
    if crate::hash::sha256(json.as_bytes()) != id {
        return Err(integrity("payload content digest mismatch"));
    }
    serde_json::from_str(&json).map_err(integrity)
}
/// Clip-table expansion used only by `migrate_legacy`, which runs at schema
/// step 16 while `version_segments` still exists (step 17 converts it into
/// manifest trees). Production reads resolve `$segments` via the manifest.
fn expand_clips(c: &Connection, version_id: &str) -> Result<Value, Error> {
    let mut stmt = c
        .prepare(
            "SELECT s.bar_json FROM version_segments vs JOIN segment_bars s ON s.segment_id=vs.segment_id \
             WHERE vs.version_id=?1 AND s.trade_date>=vs.clip_start AND s.trade_date<=vs.clip_end \
             ORDER BY vs.seq,s.trade_date",
        )
        .map_err(storage)?;
    let bars = stmt
        .query_map([version_id], |r| r.get::<_, String>(0))
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    bars.iter()
        .map(|j| serde_json::from_str(j).map_err(integrity))
        .collect::<Result<Vec<_>, _>>()
        .map(Value::Array)
}
fn resolve(
    c: &Connection,
    version_id: Option<&str>,
    v: &Value,
    records: Option<&Value>,
) -> Result<Value, Error> {
    let Some(r) = v.get(REF).and_then(Value::as_str) else {
        return Ok(v.clone());
    };
    if r == "records" {
        return records
            .cloned()
            .ok_or_else(|| integrity("envelope records reference out of context"));
    }
    if r == "segments" {
        let id =
            version_id.ok_or_else(|| integrity("envelope segments reference out of context"))?;
        let root = crate::sqlite_market::manifest::root_of(c, id)?
            .ok_or_else(|| integrity("segment-backed version lacks manifest"))?;
        return Ok(Value::Array(crate::sqlite_market::manifest::bars(c, root)?));
    }
    if let Some(id) = r.strip_prefix("payload:") {
        return fetch_payload(c, id);
    }
    if let Some(id) = r.strip_prefix("annual_bars:") {
        let Value::Array(entries) = fetch_payload(c, id)? else {
            return Err(integrity("annual payload is not an array"));
        };
        return Ok(Value::Array(
            entries.iter().map(|e| e["bar"].clone()).collect(),
        ));
    }
    Err(integrity("unknown envelope reference"))
}
/// Reconstruct the full compatibility record from a stored envelope.
/// `version_id` is required only when `records` is segment-backed.
pub(crate) fn expand(
    c: &Connection,
    version_id: Option<&str>,
    env: &Value,
) -> Result<Value, Error> {
    let mut raw = env.clone();
    let records = resolve(c, version_id, &raw["records"], None)?;
    raw["records"] = records.clone();
    if raw.pointer("/batch/bars").is_some() {
        raw["batch"]["bars"] = resolve(c, version_id, &raw["batch"]["bars"], Some(&records))?;
    }
    if let Some(v) = raw
        .pointer("/local_yearly_derivation_v1/annual/records")
        .cloned()
    {
        *raw.pointer_mut("/local_yearly_derivation_v1/annual/records")
            .unwrap() = resolve(c, version_id, &v, Some(&records))?;
    }
    Ok(raw)
}
/// Expand only when `v` is an envelope (audit rows written before this schema
/// keep full inline records).
pub(crate) fn expand_if_envelope(
    c: &Connection,
    version_id: Option<&str>,
    v: &Value,
) -> Result<Value, Error> {
    if v["records"].get(REF).is_some() {
        expand(c, version_id, v)
    } else {
        Ok(v.clone())
    }
}
/// Fail closed when a stored audit references a payload that is not present.
pub(crate) fn require_refs(c: &Connection, v: &Value) -> Result<(), Error> {
    match v {
        Value::Object(map) => {
            if let Some(r) = v.get(REF).and_then(Value::as_str) {
                if r.starts_with("payload:") || r.starts_with("annual_bars:") {
                    fetch_payload(c, r.split_once(':').unwrap().1).map(|_| ())?;
                } else if r != "records" && r != "segments" {
                    return Err(integrity("unknown audit envelope reference"));
                }
            }
            map.values().try_for_each(|x| require_refs(c, x))
        }
        Value::Array(items) => items.iter().try_for_each(|x| require_refs(c, x)),
        _ => Ok(()),
    }
}
/// One-time data migration: convert legacy `compat_json` rows (staged in
/// `envelope_json` by the schema migration) into envelopes, fill
/// `segment_bars.bar_json` from the original records, and write `payloads`
/// rows. A migrated version keeps `$segments` only when clip expansion
/// reproduces its original records exactly; otherwise its own array moves to
/// `payloads`. Runs inside the schema transaction; any failure rolls back.
pub(crate) fn migrate_legacy(c: &Connection) -> Result<(), Error> {
    let mut stmt = c
        .prepare("SELECT version_id,envelope_json FROM data_versions ORDER BY rowid")
        .map_err(storage)?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    drop(stmt);
    for (id, legacy) in rows {
        let raw: Value = serde_json::from_str(&legacy).map_err(integrity)?;
        if raw["records"].get(REF).is_some() {
            continue;
        }
        if raw["integrity_sha256"].as_str() != Some(compat::legacy_digest(&raw)?.as_str()) {
            return Err(integrity("legacy record digest mismatch during migration"));
        }
        let (env, payloads) = if raw["period"].as_str() == Some("Day") {
            fill_segment_bars(c, &id, &raw)?;
            let marker = if expand_clips(c, &id)? == raw["records"] {
                ref_segments()
            } else {
                let p = array_payload(&raw["records"])?;
                let id = p.id.clone();
                write_payloads(c, &[p])?;
                ref_payload(&id)
            };
            pack_daily(&raw, marker)?
        } else {
            pack_yearly(&raw)?
        };
        write_payloads(c, &payloads)?;
        c.execute(
            "UPDATE data_versions SET envelope_json=?1 WHERE version_id=?2",
            params![text(&env)?, id],
        )
        .map_err(storage)?;
    }
    // `version_segments` exists only while this data step runs at schema 16;
    // schema 17 drops it, so a rerun on a packed database skips this check.
    let has_clips: bool = c
        .query_row(
            "SELECT count(*)>0 FROM sqlite_master WHERE type='table' AND name='version_segments'",
            [],
            |r| r.get(0),
        )
        .map_err(storage)?;
    let unfilled = has_clips
        && c.query_row(
            "SELECT EXISTS(SELECT 1 FROM segment_bars s JOIN version_segments vs ON vs.segment_id=s.segment_id WHERE s.bar_json='')",
            [],
            |r| r.get(0),
        )
        .map_err(storage)?;
    if unfilled {
        return Err(integrity("referenced segment bars lack content"));
    }
    Ok(())
}
/// Fill `bar_json` for rows covered by this version's segment clips, but only
/// where still empty: the version that created a segment (its clip spans the
/// whole segment) always fills first in insertion order, so shared clips
/// never overwrite the creator's exact bar content.
fn fill_segment_bars(c: &Connection, version_id: &str, raw: &Value) -> Result<(), Error> {
    let Some(records) = raw["records"].as_array() else {
        return Err(integrity("legacy daily record lacks records array"));
    };
    let by_date: std::collections::HashMap<&str, &Value> = records
        .iter()
        .map(|b| (b["date"].as_str().unwrap_or(""), b))
        .collect();
    let mut stmt = c
        .prepare(
            "SELECT s.segment_id,s.trade_date FROM version_segments vs JOIN segment_bars s ON s.segment_id=vs.segment_id \
             WHERE vs.version_id=?1 AND s.trade_date>=vs.clip_start AND s.trade_date<=vs.clip_end AND s.bar_json='' \
             ORDER BY s.trade_date",
        )
        .map_err(storage)?;
    let rows = stmt
        .query_map([version_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    drop(stmt);
    for (seg, date) in rows {
        let bar = by_date
            .get(date.as_str())
            .ok_or_else(|| integrity("segment bar date absent from legacy record"))?;
        c.execute(
            "UPDATE segment_bars SET bar_json=?1 WHERE segment_id=?2 AND trade_date=?3",
            params![text(*bar)?, seg, date],
        )
        .map_err(storage)?;
    }
    Ok(())
}
