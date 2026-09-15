//! Append-only proven-gap facts. This layer stores opaque evidence identity
//! and never decides which gaps exist or verifies evidence objects; producer
//! authorization and evidence verification are application decisions.
use super::*;
use crate::market_storage::{GapCovering, ProvenGap};
impl SqliteMarket {
    /// INSERT-only fact write. Re-recording the identical gap is an
    /// idempotent no-op; the same gap_id with different content fails closed.
    pub fn record_proven_gap(&self, gap: &ProvenGap) -> Result<bool, Error> {
        gap.validate()?;
        self.write(|c| match insert_gap(c, gap) {
            Ok(()) => Ok(true),
            Err(insert) => match read_gap(c, &gap.gap_id)? {
                Some(existing) if existing == *gap => Ok(false),
                Some(_) => Err(integrity("proven gap identity conflict")),
                None => Err(insert),
            },
        })
    }
    /// Append a covering binding. The gap row and its scope are checked
    /// mechanically; re-binding the identical pair is an idempotent no-op.
    pub fn cover_gap(&self, covering: &GapCovering) -> Result<bool, Error> {
        covering.validate()?;
        self.write(|c| {
            let gap = read_gap(c, &covering.gap_id)?
                .ok_or_else(|| Error::Invalid("covering references unknown gap".into()))?;
            // A covering binds a published version only; staged/validated
            // candidates never take a gap out of the open set.
            let scope: Option<(String, String)> = c
                .query_row(
                    "SELECT v.symbol,v.period FROM data_versions v WHERE v.version_id=?1 AND (v.state='published' OR EXISTS(SELECT 1 FROM publication_batches b WHERE b.version_id=v.version_id))",
                    [&covering.version_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(storage)?;
            let Some((symbol, period)) = scope else {
                return Err(Error::Invalid(
                    "covering references unknown or unpublished version".into(),
                ));
            };
            if symbol != gap.symbol || period != super::period(gap.period) {
                return Err(Error::Invalid("covering version scope mismatch".into()));
            }
            match read_covering(c, &covering.gap_id, &covering.version_id)? {
                Some(existing) if existing == *covering => Ok(false),
                Some(_) => Err(integrity("gap covering identity conflict")),
                None => c
                    .execute(
                        "INSERT INTO gap_coverings VALUES(?1,?2,?3)",
                        params![
                            covering.gap_id,
                            covering.version_id,
                            covering.covered_at.to_rfc3339()
                        ],
                    )
                    .map_err(storage)
                    .map(|_| true),
            }
        })
    }
    pub fn proven_gap(&self, gap_id: &str) -> Result<Option<ProvenGap>, Error> {
        self.read(|c| read_gap(c, gap_id))
    }
    /// Gaps without any covering binding for one security/period.
    pub fn open_proven_gaps(&self, symbol: &str, period: Period) -> Result<Vec<ProvenGap>, Error> {
        self.read(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT g.gap_id,g.symbol,g.period,g.start_date,g.end_date,g.evidence_kind,g.evidence_id,g.evidence_digest,g.observed_at FROM proven_gaps g WHERE g.symbol=?1 AND g.period=?2 AND NOT EXISTS(SELECT 1 FROM gap_coverings c WHERE c.gap_id=g.gap_id) ORDER BY g.start_date,g.end_date,g.gap_id",
                )
                .map_err(storage)?;
            stmt.query_map(params![symbol, super::period(period)], gap_row)
                .map_err(storage)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(storage)?
                .into_iter()
                .collect()
        })
    }
    /// Audit read: all covering bindings for one gap, in append order.
    pub fn gap_coverings(&self, gap_id: &str) -> Result<Vec<GapCovering>, Error> {
        self.read(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT gap_id,version_id,covered_at FROM gap_coverings WHERE gap_id=?1 ORDER BY covered_at,version_id",
                )
                .map_err(storage)?;
            stmt.query_map([gap_id], covering_row)
                .map_err(storage)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(storage)?
                .into_iter()
                .collect()
        })
    }
}
fn insert_gap(c: &Connection, gap: &ProvenGap) -> Result<(), Error> {
    c.execute(
        "INSERT INTO proven_gaps VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![
            gap.gap_id,
            gap.symbol,
            super::period(gap.period),
            gap.start_date.to_string(),
            gap.end_date.to_string(),
            gap.evidence_kind,
            gap.evidence_id,
            gap.evidence_digest,
            gap.observed_at.to_rfc3339()
        ],
    )
    .map_err(storage)?;
    Ok(())
}
fn gap_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Result<ProvenGap, Error>> {
    let period: String = r.get(2)?;
    let start: String = r.get(3)?;
    let end: String = r.get(4)?;
    let observed_at: String = r.get(8)?;
    Ok(|| -> Result<ProvenGap, Error> {
        let gap = ProvenGap {
            gap_id: r.get(0).map_err(storage)?,
            symbol: r.get(1).map_err(storage)?,
            period: match period.as_str() {
                "day" => Period::Day,
                "year" => Period::Year,
                other => {
                    return Err(integrity(format!("unreadable gap period {other:?}")));
                }
            },
            start_date: start
                .parse()
                .map_err(|_| integrity("unreadable gap start_date"))?,
            end_date: end
                .parse()
                .map_err(|_| integrity("unreadable gap end_date"))?,
            evidence_kind: r.get(5).map_err(storage)?,
            evidence_id: r.get(6).map_err(storage)?,
            evidence_digest: r.get(7).map_err(storage)?,
            observed_at: observed_at
                .parse()
                .map_err(|_| integrity("unreadable gap observed_at"))?,
        };
        gap.validate()
            .map_err(|_| integrity("stored proven gap violates invariants"))?;
        Ok(gap)
    }())
}
fn covering_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Result<GapCovering, Error>> {
    let covered_at: String = r.get(2)?;
    Ok(|| -> Result<GapCovering, Error> {
        let covering = GapCovering {
            gap_id: r.get(0).map_err(storage)?,
            version_id: r.get(1).map_err(storage)?,
            covered_at: covered_at
                .parse()
                .map_err(|_| integrity("unreadable covering covered_at"))?,
        };
        covering
            .validate()
            .map_err(|_| integrity("stored gap covering violates invariants"))?;
        Ok(covering)
    }())
}
fn read_gap(c: &Connection, gap_id: &str) -> Result<Option<ProvenGap>, Error> {
    c.query_row(
        "SELECT gap_id,symbol,period,start_date,end_date,evidence_kind,evidence_id,evidence_digest,observed_at FROM proven_gaps WHERE gap_id=?1",
        [gap_id],
        gap_row,
    )
    .optional()
    .map_err(storage)?
    .transpose()
}
fn read_covering(
    c: &Connection,
    gap_id: &str,
    version_id: &str,
) -> Result<Option<GapCovering>, Error> {
    c.query_row(
        "SELECT gap_id,version_id,covered_at FROM gap_coverings WHERE gap_id=?1 AND version_id=?2",
        params![gap_id, version_id],
        covering_row,
    )
    .optional()
    .map_err(storage)?
    .transpose()
}
#[cfg(test)]
mod tests;
