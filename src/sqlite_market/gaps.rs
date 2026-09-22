//! Append-only proven-gap facts. This layer stores opaque evidence identity
//! and never decides which gaps exist or verifies evidence objects; producer
//! authorization and evidence verification are application decisions.
use super::*;
use crate::market_storage::{
    CONFIRMED_MISSING_RANGE, GAP_DECISION_EVIDENCE_KIND, GapCovering, GapVerificationDecision,
    ProvenGap, derive_gap_id,
};
impl SqliteMarket {
    /// Atomic gap-decision fact write (#155): the decision row and — for a
    /// `confirmed_missing_range` conclusion only — its `proven_gaps` row
    /// commit in ONE write transaction; either side failing rolls back
    /// both. Re-recording the identical decision is an idempotent no-op;
    /// the same decision_id (or derived gap_id) with different content
    /// fails closed. This is a pure fact layer: it stores the sealed
    /// decision verbatim and never decides which gaps exist.
    pub fn record_gap_decision(&self, decision: &GapVerificationDecision) -> Result<bool, Error> {
        self.write_gap_decision(decision, false)
    }
    /// Guarded variant for the trusted producer (#155 §3.2-2): the same
    /// atomic write, plus the current effective qfq head for the
    /// decision's symbol+period must still equal `subject_version_id`
    /// inside the SAME write transaction. A moved head fails closed and
    /// rolls back decision and gap together — no gap is ever committed
    /// from a stale subject snapshot.
    pub fn record_gap_decision_guarded(
        &self,
        decision: &GapVerificationDecision,
    ) -> Result<bool, Error> {
        self.write_gap_decision(decision, true)
    }
    fn write_gap_decision(
        &self,
        decision: &GapVerificationDecision,
        guard_subject_head: bool,
    ) -> Result<bool, Error> {
        self.write(|c| record_gap_decision_tx(c, decision, guard_subject_head))
    }
}

/// Connection-level gap-decision write shared by both backends. The gap
/// tables are schema-neutral; the subject-head guard resolves through the
/// schema-aware `head`.
pub(crate) fn record_gap_decision_tx(
    c: &Connection,
    decision: &GapVerificationDecision,
    guard_subject_head: bool,
) -> Result<bool, Error> {
    {
        decision.validate()?;
        let gap = ProvenGap {
            gap_id: derive_gap_id(decision),
            symbol: decision.symbol.clone(),
            period: decision.period,
            start_date: decision.start_date,
            end_date: decision.end_date,
            evidence_kind: GAP_DECISION_EVIDENCE_KIND.into(),
            evidence_id: decision.decision_id.clone(),
            evidence_digest: decision.decision_sha256.clone(),
            observed_at: decision.decided_at,
        };
        gap.validate()?;
        {
            let inserted = match insert_decision(c, decision) {
                Ok(()) => true,
                Err(insert) => match read_decision(c, &decision.decision_id)? {
                    Some(existing) if existing == *decision => false,
                    Some(_) => {
                        return Err(integrity("gap decision identity conflict"));
                    }
                    None => return Err(insert),
                },
            };
            // The stale-head guard gates only a NEW commit; an identical
            // already-committed decision replays as a no-op even if the
            // head has since moved.
            if guard_subject_head && inserted {
                let scope = Scope {
                    symbol: decision.symbol.clone(),
                    period: decision.period,
                };
                if head(c, &scope)?.as_deref() != Some(decision.subject_version_id.as_str()) {
                    return Err(integrity("gap decision subject head moved"));
                }
            }
            if decision.conclusion == CONFIRMED_MISSING_RANGE {
                match insert_gap(c, &gap) {
                    Ok(()) => Ok(true),
                    Err(insert) => match read_gap(c, &gap.gap_id)? {
                        Some(existing) if existing == gap => Ok(inserted),
                        Some(_) => Err(integrity("proven gap identity conflict")),
                        None => Err(insert),
                    },
                }
            } else {
                Ok(inserted)
            }
        }
    }
}
impl SqliteMarket {
    /// Audit read of one stored decision; the stored row is mechanically
    /// re-validated and integrity fail-closed when unreadable.
    pub fn gap_verification_decision(
        &self,
        decision_id: &str,
    ) -> Result<Option<GapVerificationDecision>, Error> {
        self.read(|c| read_decision(c, decision_id))
    }
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
        self.read(|c| read_open_gaps(c, symbol, period))
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
fn insert_decision(c: &Connection, d: &GapVerificationDecision) -> Result<(), Error> {
    c.execute(
        "INSERT INTO gap_verification_decisions VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
        params![
            d.decision_id,
            d.symbol,
            super::period(d.period),
            d.start_date.to_string(),
            d.end_date.to_string(),
            d.subject_version_id,
            d.subject_version_digest,
            d.conclusion,
            d.basis_kind,
            d.basis_ref,
            d.basis_sha256,
            d.verifier_id,
            d.decided_at.to_rfc3339(),
            d.decision_sha256,
        ],
    )
    .map_err(storage)?;
    Ok(())
}
fn decision_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Result<GapVerificationDecision, Error>> {
    let period: String = r.get(2)?;
    let start: String = r.get(3)?;
    let end: String = r.get(4)?;
    let decided_at: String = r.get(12)?;
    Ok(|| -> Result<GapVerificationDecision, Error> {
        let decision = GapVerificationDecision {
            decision_id: r.get(0).map_err(storage)?,
            symbol: r.get(1).map_err(storage)?,
            period: match period.as_str() {
                "day" => Period::Day,
                other => {
                    return Err(integrity(format!("unreadable decision period {other:?}")));
                }
            },
            start_date: start
                .parse()
                .map_err(|_| integrity("unreadable decision start_date"))?,
            end_date: end
                .parse()
                .map_err(|_| integrity("unreadable decision end_date"))?,
            subject_version_id: r.get(5).map_err(storage)?,
            subject_version_digest: r.get(6).map_err(storage)?,
            conclusion: r.get(7).map_err(storage)?,
            basis_kind: r.get(8).map_err(storage)?,
            basis_ref: r.get(9).map_err(storage)?,
            basis_sha256: r.get(10).map_err(storage)?,
            verifier_id: r.get(11).map_err(storage)?,
            decided_at: decided_at
                .parse()
                .map_err(|_| integrity("unreadable decision decided_at"))?,
            decision_sha256: r.get(13).map_err(storage)?,
        };
        decision
            .validate()
            .map_err(|_| integrity("stored gap decision violates invariants"))?;
        Ok(decision)
    }())
}
pub(crate) fn read_decision(
    c: &Connection,
    decision_id: &str,
) -> Result<Option<GapVerificationDecision>, Error> {
    c.query_row(
        "SELECT decision_id,symbol,period,start_date,end_date,subject_version_id,subject_version_digest,conclusion,basis_kind,basis_ref,basis_sha256,verifier_id,decided_at,decision_sha256 FROM gap_verification_decisions WHERE decision_id=?1",
        [decision_id],
        decision_row,
    )
    .optional()
    .map_err(storage)?
    .transpose()
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
/// Same-transaction read of gaps without any covering binding; callers that
/// already hold a read snapshot use this instead of `open_proven_gaps`.
pub(crate) fn read_open_gaps(
    c: &Connection,
    symbol: &str,
    period: Period,
) -> Result<Vec<ProvenGap>, Error> {
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
