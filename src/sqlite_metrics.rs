//! Reads a fixed verified yearly and hash-bound fixture coverage in one snapshot.
use crate::{
    data_supply::wire::{Dataset, VersionRef},
    market_storage::Error,
    metrics::{
        consecutive::{Completion, Observation},
        service::InputRepository,
        wire::Years,
    },
    sqlite_market::{backend::MarketBackend, read_record},
};
use chrono::Datelike;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AnalysisCoverage {
    pub origin: Origin,
    pub years: Vec<YearCoverage>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub enum Origin {
    #[serde(rename = "fixture")]
    Fixture,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct YearCoverage {
    pub year: i32,
    pub status: Coverage,
    pub evidence_ref: String,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Coverage {
    Complete,
    Incomplete,
    Unknown,
}
#[derive(Clone)]
pub struct SqliteMetrics {
    pub market: MarketBackend,
}
fn integrity(e: impl std::fmt::Display) -> Error {
    Error::Integrity(e.to_string())
}
impl InputRepository for SqliteMetrics {
    fn observations(
        &self,
        reference: &VersionRef,
        range: &Years,
    ) -> Result<Vec<Observation>, Error> {
        self.market.read(|c| {
            let r = read_record(c, &reference.version_id, true, true)?;
            let v = r.version();
            if reference.dataset != Dataset::Yearly
                || v.period != crate::market_data::Period::Year
                || reference.entity.as_ref().map(|e| e.symbol()).as_deref()
                    != Some(v.symbol.as_str())
                || r.raw()["integrity_sha256"] != reference.integrity_sha256
            {
                return Err(integrity(
                    "fixed computation version identity/digest mismatch",
                ));
            }
            let mut coverage = std::collections::BTreeMap::new();
            // Derived-completeness (issue #225, D2=C): for production yearly
            // versions without fixture coverage, the embedded
            // local_yearly_derivation_v1.evidence is the only completion basis —
            // mechanical, integrity_sha256-bound, no new issuer.
            let mut derived: Option<(std::collections::BTreeSet<i32>, bool)> = None;
            if let Some(e) = r.raw().get("analysis_coverage_v1") {
                if !v.source.starts_with("fixture:") {
                    return Err(integrity(
                        "fixture coverage cannot certify a production source",
                    ));
                }
                let evidence: AnalysisCoverage =
                    serde_json::from_value(e.clone()).map_err(integrity)?;
                if evidence.years.len() > 1000 {
                    return Err(Error::Invalid("coverage evidence budget exceeded".into()));
                }
                for y in evidence.years {
                    if y.evidence_ref.trim().is_empty()
                        || !v.records.iter().any(|r| r.date.year() == y.year)
                        || coverage.insert(y.year, y.status).is_some()
                    {
                        return Err(integrity("coverage evidence year/ref mismatch"));
                    }
                }
            } else if let Some(d) = r.raw().get("local_yearly_derivation_v1") {
                #[derive(Deserialize)]
                struct EmbeddedEvidence {
                    years: std::collections::BTreeSet<i32>,
                    coverage: crate::market_data::CoverageEvidence,
                }
                let ev: EmbeddedEvidence = serde_json::from_value(
                    d.get("evidence")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null),
                )
                .map_err(integrity)?;
                if ev.years.len() > 1000 {
                    return Err(Error::Invalid("coverage evidence budget exceeded".into()));
                }
                if ev.coverage.status != crate::market_data::CoverageStatus::Unknown {
                    derived = Some((
                        ev.years,
                        ev.coverage.status == crate::market_data::CoverageStatus::Complete,
                    ));
                }
            }
            let mut observations = vec![];
            for b in v
                .records
                .iter()
                .filter(|b| b.date.year() >= range.start_year && b.date.year() <= range.end_year)
            {
                let (first, last, unfinished) = crate::sqlite_supply::yearly_row_meta(c, &r, b)?;
                let (first, last, finished): (String, String, bool) = (
                    first.unwrap().to_string(),
                    last.unwrap().to_string(),
                    !unfinished.unwrap(),
                );
                let completion = if let Some((years, complete)) = &derived {
                    // D1=A: Complete requires verifier-certified base coverage of
                    // the year AND a finished year; certified-but-open or
                    // explicitly partial reads Incomplete, absent years Unknown.
                    if years.contains(&b.date.year()) {
                        if *complete && finished {
                            Completion::Complete
                        } else {
                            Completion::Incomplete
                        }
                    } else {
                        Completion::Unknown
                    }
                } else {
                    match coverage.get(&b.date.year()) {
                        Some(Coverage::Complete) => Completion::Complete,
                        Some(Coverage::Incomplete) => Completion::Incomplete,
                        _ => Completion::Unknown,
                    }
                };
                observations.push(Observation {
                    year: b.date.year(),
                    first: first.parse().map_err(integrity)?,
                    last: last.parse().map_err(integrity)?,
                    close: b.close,
                    unfinished: !finished,
                    completion,
                });
                if observations.len() > 1000 {
                    return Err(Error::Invalid("observation budget exceeded".into()));
                }
            }
            Ok(observations)
        })
    }
}
