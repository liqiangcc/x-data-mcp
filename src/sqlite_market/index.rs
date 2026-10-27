//! Index benchmark storage (issue-383 §3.9/§6.3, M1 PR-5): the per-index
//! unit/aggregation metadata table and the commit of an index bar series.
//!
//! Layer position (separation points 1/2/6): the registry
//! (`crate::index_registry`) says **which** series are benchmarks and **how**
//! their volume/amount were aggregated; the adapter (`official_market`) hands
//! over neutral facts (bars, units, coverage, member digests); this module
//! only persists them. No source name, URL or file name appears here, and no
//! per-index branch exists — the row is written from the registration.
//!
//! An index is an ordinary bar series: `market_series(instrument_class=1,
//! adjustment=0, price_scale=2)` + `series_bars` + `market_events`, through
//! the same commit path as every other series (§6.3). It never gets a forward
//! series or a yearly derivation, and the public MCP read surface never
//! resolves it (AC-M1-10).

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

use super::{evidence_store, integrity, series, series_commit, storage};
use crate::compact_market::{self, Digest, EvidenceKind, ExcludedBar, VersionEvidence};
use crate::index_registry::{self, IndexRegistration};
use crate::market_data::{Adjustment, Bar, IndexBatch, Period, UnitCurrencyEvidence};
use crate::market_storage::{Error, Receipt, Scope};

/// Durable `algorithm_version` of an index daily series. The derivation is
/// "promote the `.day` member verbatim at 0.01 点, no adjustment, dropping
/// bars whose own OHLC ordering is impossible" (#383 owner ruling 2026-10-27);
/// a later change to that mapping must bump this label.
pub(crate) const INDEX_ALGORITHM: &str = "index-daily-v2";

fn invalid(message: impl std::fmt::Display) -> Error {
    Error::Invalid(message.to_string())
}

/// One `index_series_meta` row as read back (§3.9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IndexMetaRow {
    pub series_key: i64,
    pub aliases: String,
    pub aggregate_scope: String,
    pub member_count: Option<i64>,
    pub price_unit: String,
    pub volume_unit: String,
    pub amount_unit: String,
    pub evidence_digest: Digest,
    pub evidence_note: String,
}

const META_COLUMNS: &str = "series_key,aliases,aggregate_scope,member_count,price_unit,\
     volume_unit,amount_unit,evidence_digest,evidence_note";
const META_COLUMNS_QUALIFIED: &str = "m.series_key,m.aliases,m.aggregate_scope,m.member_count,\
     m.price_unit,m.volume_unit,m.amount_unit,m.evidence_digest,m.evidence_note";

type RawMeta = (
    i64,
    String,
    String,
    Option<i64>,
    String,
    String,
    String,
    Vec<u8>,
    String,
);

fn meta_row(raw: RawMeta) -> Result<IndexMetaRow, Error> {
    Ok(IndexMetaRow {
        series_key: raw.0,
        aliases: raw.1,
        aggregate_scope: raw.2,
        member_count: raw.3,
        price_unit: raw.4,
        volume_unit: raw.5,
        amount_unit: raw.6,
        evidence_digest: raw
            .7
            .as_slice()
            .try_into()
            .map_err(|_| integrity("index evidence_digest is not 32 bytes"))?,
        evidence_note: raw.8,
    })
}

/// The metadata row of one series, `None` when it was never written.
pub(crate) fn read_meta(c: &Connection, series_key: i64) -> Result<Option<IndexMetaRow>, Error> {
    c.query_row(
        &format!("SELECT {META_COLUMNS} FROM index_series_meta WHERE series_key=?1"),
        params![series_key],
        |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
            ))
        },
    )
    .optional()
    .map_err(storage)?
    .map(meta_row)
    .transpose()
}

/// The metadata row of one index entity — the accessor the research read
/// model (PR-8) and the registry-consistency test consume.
pub(crate) fn read_meta_by_entity(
    c: &Connection,
    entity: &str,
) -> Result<Option<IndexMetaRow>, Error> {
    c.query_row(
        &format!(
            "SELECT {META_COLUMNS_QUALIFIED} FROM index_series_meta m \
             JOIN market_series s ON s.series_key=m.series_key WHERE s.entity=?1"
        ),
        params![entity],
        |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
            ))
        },
    )
    .optional()
    .map_err(storage)?
    .map(meta_row)
    .transpose()
}

/// One index write: the adapter's neutral facts plus the identity of the task
/// item and generation that produced them.
///
/// The generation fields are the caller's (the worker resolves the generation
/// for the run, PR-7); this module never invents them.
pub(crate) struct IndexWrite<'a> {
    pub index: &'a IndexBatch,
    pub result_id: &'a str,
    pub task_id: &'a str,
    pub item_id: &'a str,
    pub run_id: &'a str,
    pub expected_task_revision: u64,
    pub generation_id: &'a str,
    pub generation_manifest_sha256: &'a str,
    pub generation_evidence: &'a Value,
    pub published_at: DateTime<Utc>,
}

/// Commit one registered index series and its `index_series_meta` row in the
/// caller's transaction.
///
/// Gates, in order: the entity must be registered; the registration must
/// carry unit/scope evidence (AC-M1-6: no evidence, no row); the batch must be
/// daily 不复权 with the registered units; then the ordinary bar-series commit
/// runs with the index shape. The metadata row is rewritten from the
/// registration on every commit, so a changed registration (a new alias, a
/// corrected scope) lands with the next index update instead of drifting
/// silently.
pub(crate) fn commit_index(c: &Connection, w: &IndexWrite<'_>) -> Result<Receipt, Error> {
    let batch = &w.index.batch;
    let registration = index_registry::lookup(&batch.symbol).ok_or_else(|| {
        invalid(format!(
            "{} is not a registered index benchmark",
            batch.symbol
        ))
    })?;
    registration.require_evidence().map_err(invalid)?;
    if batch.period != Period::Day || batch.adjustment != Adjustment::Raw {
        return Err(integrity(
            "index series are daily 不复权 only (adjustment=0, no yearly)",
        ));
    }
    require_index_units(registration, &batch.unit_currency)?;
    if batch.bars.is_empty() {
        return Err(integrity("index batch carries no bars"));
    }
    let (valid, excluded) = split_invalid_ohlc(&batch.bars);
    if valid.is_empty() {
        return Err(integrity(
            "index batch carries no valid bar after OHLC ordering",
        ));
    }
    let bars =
        series_commit::to_compact_bars(&valid, Period::Day, compact_market::PRICE_SCALE_CENTS)?;
    let head = series::read_series_adjusted(c, &batch.symbol, Period::Day, Adjustment::Raw)?;
    let change = classify(c, &batch.symbol, head.as_ref(), &bars)?;
    let range_start_day = bars[0].trade_day;
    let range_end_day = bars[bars.len() - 1].trade_day;
    let binding = series_commit::put_generation_binding(
        c,
        w.generation_id,
        w.generation_manifest_sha256,
        &w.index.day_member_sha256,
        &w.index.security_event_sha256,
        w.generation_evidence,
    )?;
    let version_evidence = evidence_store::put_evidence(
        c,
        EvidenceKind::Version,
        &json!(VersionEvidence {
            source: batch.provenance.source.clone(),
            coverage_reason: batch.coverage.reason.clone(),
            unit_currency: batch.unit_currency.clone(),
            adjustment_evidence: batch.adjustment_evidence.clone(),
            excluded_bars: (!excluded.is_empty()).then_some(excluded),
        }),
    )?;
    let scope = Scope {
        symbol: batch.symbol.clone(),
        period: Period::Day,
    };
    let occupancy_key = scope.occupancy_key();
    let receipt = series_commit::commit_index_tx(
        c,
        &series_commit::Commit {
            result_id: w.result_id,
            task_id: w.task_id,
            item_id: w.item_id,
            run_id: w.run_id,
            occupancy_key: &occupancy_key,
            expected_task_revision: w.expected_task_revision,
            entity: &batch.symbol,
            period: Period::Day,
            adjustment: Adjustment::Raw,
            base_version_id: head.as_ref().map(|row| row.version_id.as_str()),
            change,
            range_start_day,
            range_end_day,
            algorithm_version: INDEX_ALGORITHM,
            window_digest: Some(series_commit::window_fold(&bars)?),
            source_binding_digest: Some(binding),
            evidence_digest: Some(version_evidence),
            coverage_status: Some(batch.coverage.status),
            metadata_changed: false,
            event_evidence_digest: version_evidence,
            published_at: Some(w.published_at),
            staged_version_id: None,
            defer_task_row: false,
        },
    )?;
    let series = series::read_series_adjusted(c, &batch.symbol, Period::Day, Adjustment::Raw)?
        .ok_or_else(|| integrity("index commit left no series row"))?;
    write_meta(c, registration, &series, binding, w.index)?;
    Ok(receipt)
}

/// Split an index batch into the bars whose OHLC ordering is possible and the
/// bars excluded from the series (#383 owner ruling 2026-10-27, option a).
///
/// The rule is only about the bar's own four prices — `low <= min(open, close)`,
/// `max(open, close) <= high`, `low <= high`. An excluded bar is not repaired,
/// clamped or estimated: it never reaches `series_bars`, while every other bar
/// is committed as usual. The caller records `excluded` in the version evidence
/// so the decision is deterministic and reproducible from the stored bundle.
///
/// This is deliberately reachable only from the index commit: a stock series
/// still fails closed in `series_commit::to_compact_bars`.
fn split_invalid_ohlc(bars: &[Bar]) -> (Vec<Bar>, Vec<ExcludedBar>) {
    let mut valid = Vec::with_capacity(bars.len());
    let mut excluded = Vec::new();
    for bar in bars {
        if bar.low <= bar.open
            && bar.low <= bar.close
            && bar.low <= bar.high
            && bar.high >= bar.open
            && bar.high >= bar.close
        {
            valid.push(bar.clone());
            continue;
        }
        excluded.push(ExcludedBar {
            date: bar.date,
            open: bar.open,
            high: bar.high,
            low: bar.low,
            close: bar.close,
        });
    }
    (valid, excluded)
}

/// The unit gate (AC-M1-6): the batch must carry exactly the units the
/// registration declares. The price *currency* is deliberately not required —
/// an index level is a point value and the adapter states that explicitly.
fn require_index_units(
    registration: &IndexRegistration,
    unit: &UnitCurrencyEvidence,
) -> Result<(), Error> {
    for (label, evidence, expected) in [
        ("price_unit", &unit.price_unit, registration.price_unit),
        ("volume_unit", &unit.volume_unit, registration.volume_unit),
        ("amount_unit", &unit.amount_unit, registration.amount_unit),
    ] {
        if !evidence.is_confirmed() || evidence.value.as_deref() != Some(expected) {
            return Err(integrity(format!(
                "index {label} evidence is not the registered {expected}"
            )));
        }
    }
    if !unit.amount_currency.is_confirmed() {
        return Err(integrity("index amount currency evidence is missing"));
    }
    Ok(())
}

/// Classify the incoming bars against the stored series exactly like the
/// daily envelope path: identical content ⇒ no_change, a longer stored prefix
/// ⇒ append, anything else ⇒ replace. The index series is rewritten whole on
/// a source revision, which the event log records as `replace`.
fn classify<'a>(
    c: &Connection,
    entity: &str,
    head: Option<&series::SeriesRow>,
    bars: &'a [compact_market::CompactBar],
) -> Result<series_commit::SeriesChange<'a>, Error> {
    let Some(row) = head else {
        return Ok(series_commit::SeriesChange::FirstPublish { bars });
    };
    let stored = series::all_bars(c, row.series_key)?;
    if series::fold_bars(entity, Period::Day, row.shape(), bars)? == row.content_digest {
        return Ok(series_commit::SeriesChange::NoChange);
    }
    if stored.len() < bars.len() && stored[..] == bars[..stored.len()] {
        return Ok(series_commit::SeriesChange::Append {
            upserts: &bars[stored.len()..],
        });
    }
    Ok(series_commit::SeriesChange::Replace { bars })
}

/// Write (or refresh) the index metadata row and its `derived` evidence
/// bundle (§3.9/§6.3). The evidence records the unit/scope claim, how it was
/// proven, and the source binding the claim belongs to; it deliberately does
/// not carry the series `version_id`, because the proof is about units and
/// scope, not about one content revision.
fn write_meta(
    c: &Connection,
    registration: &IndexRegistration,
    series: &series::SeriesRow,
    source_binding_digest: Digest,
    index: &IndexBatch,
) -> Result<(), Error> {
    let evidence = json!({
        "dataset": "index_series_meta",
        "entity": registration.entity,
        "aliases": registration.aliases,
        "aggregate_scope": registration.aggregate_scope.as_str(),
        "member_count": registration.member_count,
        "price_unit": registration.price_unit,
        "volume_unit": registration.volume_unit,
        "amount_unit": registration.amount_unit,
        "price_scale": series.price_scale,
        "instrument_class": series.instrument_class,
        "adjustment": series.adjustment.as_str(),
        "evidence_note": registration.evidence_note,
        // §6.3: the unit/scope claim belongs to this exact `.day` member and
        // to the (empty) corporate-action set bound on the series row.
        "source_binding_digest": compact_market::digest_hex(&source_binding_digest),
        "day_member_sha256": index.day_member_sha256,
        "security_event_sha256": index.security_event_sha256,
        "corporate_action_set": "empty",
    });
    let evidence_digest = evidence_store::put_evidence(c, EvidenceKind::Derived, &evidence)?;
    let now = Utc::now().to_rfc3339();
    c.execute(
        "INSERT INTO index_series_meta(series_key,aliases,aggregate_scope,member_count,\
         price_unit,volume_unit,amount_unit,evidence_digest,evidence_note,created_at) \
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) \
         ON CONFLICT(series_key) DO UPDATE SET aliases=excluded.aliases,\
         aggregate_scope=excluded.aggregate_scope,member_count=excluded.member_count,\
         price_unit=excluded.price_unit,volume_unit=excluded.volume_unit,\
         amount_unit=excluded.amount_unit,evidence_digest=excluded.evidence_digest,\
         evidence_note=excluded.evidence_note",
        params![
            series.series_key,
            registration.aliases_text(),
            registration.aggregate_scope.as_str(),
            registration.member_count,
            registration.price_unit,
            registration.volume_unit,
            registration.amount_unit,
            evidence_digest.as_slice(),
            registration.evidence_note,
            now,
        ],
    )
    .map_err(storage)?;
    Ok(())
}

#[cfg(test)]
mod tests;
