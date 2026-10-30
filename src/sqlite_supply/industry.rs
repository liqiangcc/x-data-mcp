//! `market.industry_assignments` at the v8 entry (M3 PR-7, #446 §5.5).
//!
//! This is an adapter, not a second read model: the answers come from
//! [`crate::research::industry`], and this file only projects them into
//! [`Row::Fact`] cells and names the dataset version. The M3 tables have no
//! registration row, so the version is the content summary recomputed from the
//! snapshot/interval rows; the same rows always produce the same id, and a
//! pinned id that no longer matches is `integrity_failure`, not a silent pass.
//!
//! `assigned` rows carry the interval fields. `unknown{before_coverage}` and
//! `unknown{missing_in_snapshot}` rows are still rows: AC-M3-3 requires the
//! unknown to be observable, and an empty result would read as "no industry"
//! instead of "not knowable".

use chrono::NaiveDate;
use rusqlite::Connection;

use crate::data_supply::query::{QueryPlan, QuerySnapshot};
use crate::data_supply::wire::{
    ContractVersion, Dataset, FactCells, FactValue, ManifestEntry, ManifestStatus, Metadata, Row,
    Security, VersionInfo, VersionRef,
};
use crate::market_storage::Error;
use crate::research::industry::{
    AssignedIndustry, IndustryAssignment, IndustryIdentity, IndustryRequest, UnknownIndustry,
};
use crate::research::temporal::{Temporal, TemporalReport};

struct Identity {
    version_id: String,
    published_at: chrono::DateTime<chrono::Utc>,
    coverage_start: Option<NaiveDate>,
    snapshot_rows: usize,
    assignment_rows: usize,
}

pub(super) fn query(
    c: &Connection,
    plan: &QueryPlan,
    _access: &crate::data_supply::Access,
) -> Result<QuerySnapshot, Error> {
    let Some(temporal) = plan.temporal else {
        return Err(invalid("industry assignments requires a resolved as_of"));
    };
    let mut result = QuerySnapshot {
        manifest: vec![],
        rows: vec![],
        temporal: None,
    };
    let Some(identity) = identity(c)? else {
        result.manifest.push(super::absent(None));
        return Ok(result);
    };
    result.manifest.push(manifest(&identity));
    if plan.entities.is_empty() {
        // An empty universe selection is not "the whole table"; answer with the
        // manifest and no rows, like the other entity-bound datasets.
        result.temporal = Some(super::m2::temporal_block(
            Dataset::IndustryAssignments,
            &TemporalReport::new(Temporal {
                as_of: temporal.as_of,
                known_at: temporal.known_at,
            }),
        ));
        return Ok(result);
    }

    let request = IndustryRequest {
        temporal: Temporal {
            as_of: temporal.as_of,
            known_at: temporal.known_at,
        },
        entities: plan
            .entities
            .iter()
            .map(Security::symbol)
            .collect::<Vec<_>>(),
    };
    let report = crate::research::industry::assignments(c, &request)
        .map_err(|error| Error::Storage(error.to_string()))?;
    let in_window = match plan.args.filter {
        crate::data_supply::wire::Filter::Daily {
            start_date,
            end_date,
            ..
        } => start_date <= temporal.as_of && temporal.as_of <= end_date,
        _ => false,
    };
    if in_window {
        for assignment in report.assignments {
            result.rows.push(fact_row(assignment, temporal.as_of)?);
            super::m2::check_budget(&result)?;
        }
    }
    result.temporal = Some(super::m2::temporal_block(
        Dataset::IndustryAssignments,
        &report.temporal,
    ));
    Ok(result)
}

pub(super) fn versions(c: &Connection, _entities: &[Security]) -> Result<Vec<VersionInfo>, Error> {
    let Some(identity) = identity(c)? else {
        return Ok(vec![]);
    };
    Ok(vec![VersionInfo {
        version_ref: reference(&identity),
        published_at: identity.published_at,
    }])
}

pub(super) fn version_ref(c: &Connection, reference: &VersionRef) -> Result<VersionRef, Error> {
    let Some(identity) = identity(c)? else {
        return Err(Error::Integrity(
            "industry assignment dataset vanished".into(),
        ));
    };
    Ok(VersionRef {
        dataset: Dataset::IndustryAssignments,
        contract_version: ContractVersion::V1,
        entity: reference.entity.clone(),
        version_id: identity.version_id.clone(),
        integrity_sha256: identity.version_id,
    })
}

fn identity(c: &Connection) -> Result<Option<Identity>, Error> {
    let identity: IndustryIdentity = crate::research::industry::dataset_identity(c)
        .map_err(|error| Error::Storage(error.to_string()))?;
    if identity.snapshot_rows == 0 {
        if identity.assignment_rows > 0 {
            return Err(Error::Integrity(
                "industry_assignments has rows without an industry_snapshots parent".into(),
            ));
        }
        return Ok(None);
    }
    let latest = identity.latest_ingested_at.as_deref().ok_or_else(|| {
        Error::Integrity("industry_snapshots has no ingested_at for its version".into())
    })?;
    Ok(Some(Identity {
        version_id: identity.version_id,
        published_at: super::series_timestamp(latest)?,
        coverage_start: identity.coverage_start,
        snapshot_rows: identity.snapshot_rows,
        assignment_rows: identity.assignment_rows,
    }))
}

fn reference(identity: &Identity) -> VersionRef {
    VersionRef {
        dataset: Dataset::IndustryAssignments,
        contract_version: ContractVersion::V1,
        entity: None,
        version_id: identity.version_id.clone(),
        integrity_sha256: identity.version_id.clone(),
    }
}

fn manifest(identity: &Identity) -> ManifestEntry {
    let coverage = match identity.coverage_start {
        Some(day) => day.to_string(),
        None => "no interval-defining snapshot".into(),
    };
    ManifestEntry {
        entity: None,
        version_ref: Some(reference(identity)),
        status: ManifestStatus::Present,
        metadata: Some(Metadata {
            source: "derived:industry_snapshot".into(),
            published_at: identity.published_at,
            coverage_status: "partial".into(),
            coverage_reason: format!(
                "industry assignments cover {} snapshot(s) and {} interval row(s), starting {}; \
                 earlier as_of answers unknown(before_coverage)",
                identity.snapshot_rows, identity.assignment_rows, coverage
            ),
            units: vec![],
            adjustment: None,
            lineage: None,
            index: None,
        }),
        stale: None,
        as_of_day: None,
        reason: None,
    }
}

fn fact_row(assignment: IndustryAssignment, as_of: NaiveDate) -> Result<Row, Error> {
    match assignment {
        IndustryAssignment::Assigned(row) => Ok(Row::Fact {
            entity: Some(entity(&row)?),
            day: row.effective_from,
            cells: assigned_cells(row),
        }),
        IndustryAssignment::Unknown(row) => Ok(Row::Fact {
            entity: Some(entity_unknown(&row)?),
            day: as_of,
            cells: unknown_cells(row),
        }),
    }
}

fn entity(row: &AssignedIndustry) -> Result<Security, Error> {
    Security::parse(&row.entity).map_err(Error::Integrity)
}

fn entity_unknown(row: &UnknownIndustry) -> Result<Security, Error> {
    Security::parse(&row.entity).map_err(Error::Integrity)
}

fn assigned_cells(row: AssignedIndustry) -> FactCells {
    cells(vec![
        ("status", text("assigned")),
        ("reason", FactValue::Null),
        ("scheme", text(row.scheme)),
        ("level", FactValue::Int(row.level)),
        ("industry_code", text(row.industry_code)),
        ("name", row.name.map_or(FactValue::Null, FactValue::Text)),
        ("name_status", text(row.name_status)),
        ("effective_from", date(Some(row.effective_from))),
        ("effective_to", date(row.effective_to)),
        ("known_at", date(Some(row.known_at))),
        ("effective_from_basis", text(row.effective_from_basis)),
        ("evidence_grade", text(row.evidence_grade)),
        ("source_snapshot_sha256", text(row.source_snapshot_sha256)),
    ])
}

fn unknown_cells(row: UnknownIndustry) -> FactCells {
    cells(vec![
        ("status", text("unknown")),
        ("reason", text(row.reason.as_str())),
        ("scheme", FactValue::Null),
        ("level", FactValue::Null),
        ("industry_code", FactValue::Null),
        ("name", FactValue::Null),
        ("name_status", FactValue::Null),
        ("effective_from", FactValue::Null),
        ("effective_to", FactValue::Null),
        ("known_at", FactValue::Null),
        ("effective_from_basis", FactValue::Null),
        ("evidence_grade", FactValue::Null),
        ("source_snapshot_sha256", FactValue::Null),
    ])
}

fn date(day: Option<NaiveDate>) -> FactValue {
    FactValue::day(day)
}

fn text(value: impl Into<String>) -> FactValue {
    FactValue::Text(value.into())
}

fn cells(pairs: Vec<(&'static str, FactValue)>) -> FactCells {
    pairs
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value))
        .collect()
}

fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}
