//! Stage C reads: resolve every selected object under one real SQLite transaction.
use crate::{
    data_supply::{Access, query::*, wire::*},
    market_storage::{Error, Scope},
    sqlite_catalog,
    sqlite_market::{
        backend::MarketBackend, compact_versions::VerifyCtx, read_record, read_record_slim,
        slim_published,
    },
};
use chrono::{DateTime, Datelike, Utc};
use rusqlite::{Connection, params};
#[derive(Clone)]
pub struct SqliteSupply {
    pub market: MarketBackend,
}
fn storage(e: impl std::fmt::Display) -> Error {
    Error::Storage(e.to_string())
}
fn market_ref(
    dataset: Dataset,
    entity: &Security,
    r: &crate::market_storage::compat::CheckedRecord,
) -> VersionRef {
    VersionRef {
        dataset,
        contract_version: ContractVersion::V1,
        entity: Some(entity.clone()),
        version_id: r.version().version_id.clone(),
        integrity_sha256: r.raw()["integrity_sha256"].as_str().unwrap().into(),
    }
}
/// `market_ref` built from committed-tier row facts (XP-020): the
/// envelope `integrity_sha256` of a published row IS the stored
/// `integrity_digest` hex — the same value `synthesize` emits. A
/// missing digest is committed-state corruption and fails closed.
fn market_ref_facts(
    dataset: Dataset,
    entity: &Security,
    facts: &crate::sqlite_market::compact_versions::VersionResultFacts,
) -> Result<VersionRef, Error> {
    Ok(VersionRef {
        dataset,
        contract_version: ContractVersion::V1,
        entity: Some(entity.clone()),
        version_id: facts.version_id.clone(),
        integrity_sha256: crate::compact_market::digest_hex(
            &facts.integrity_digest.ok_or_else(|| {
                Error::Integrity("committed version lacks integrity digest".into())
            })?,
        ),
    })
}
fn catalog_ref(v: &crate::catalog::CatalogVersion) -> Result<VersionRef, Error> {
    Ok(VersionRef {
        dataset: Dataset::Catalog,
        contract_version: ContractVersion::V1,
        entity: None,
        version_id: v.version_id.clone(),
        integrity_sha256: sqlite_catalog::digest(v)?,
    })
}
/// `read_market` backed by the header-only compact projection when the row
/// is a published version — identical `raw()`/identity surface with
/// `version().records` empty. `None` (non-compact, missing, or unpublished
/// row) falls back to the unchanged full path; `Some` callers must not read
/// bars from the record.
fn slim_market(
    c: &Connection,
    ctx: &mut VerifyCtx,
    dataset: Dataset,
    entity: &Security,
    id: &str,
) -> Result<Option<crate::market_storage::compat::CheckedRecord>, Error> {
    let Some(r) = slim_published(c, ctx, id, true, true)? else {
        return Ok(None);
    };
    if r.version().symbol != entity.symbol() || Some(r.version().period) != dataset.period() {
        return Err(Error::Integrity("version entity/dataset mismatch".into()));
    }
    Ok(Some(r))
}
fn read_market(
    c: &Connection,
    ctx: &mut VerifyCtx,
    dataset: Dataset,
    entity: &Security,
    id: &str,
) -> Result<crate::market_storage::compat::CheckedRecord, Error> {
    if let Some(r) = slim_market(c, ctx, dataset, entity, id)? {
        return Ok(r);
    }
    let r = read_record(c, id, true, true)?;
    if r.version().symbol != entity.symbol() || Some(r.version().period) != dataset.period() {
        return Err(Error::Integrity("version entity/dataset mismatch".into()));
    }
    Ok(r)
}
/// `get_yearly_base_years` per-item resolution (XP-014): the pinned daily
/// version is fully verified inside this read transaction and the in-window
/// year set is derived from the verified records — never inferred from
/// first/last/count and never satisfied by stored digests. Missing or
/// unpublished bases report `base_unavailable`; identity/period/content/
/// lineage failures report `integrity_failure`; storage failures abort the
/// whole request. Non-compact schemas delegate to `read_record` verbatim.
fn presence_item(
    c: &Connection,
    ctx: &mut VerifyCtx,
    compact: bool,
    item: &YearPresenceItem,
) -> Result<crate::data_supply::presence::PresenceOutcome, Error> {
    use crate::data_supply::presence::PresenceOutcome as Outcome;
    let first = chrono::NaiveDate::from_ymd_opt(item.start_year, 1, 1)
        .ok_or_else(|| invalid("invalid start_year"))?;
    let last = chrono::NaiveDate::from_ymd_opt(item.end_year, 12, 31)
        .ok_or_else(|| invalid("invalid end_year"))?;
    let resolved = |integrity_sha256: String, version: &crate::publish::EffectiveVersion| {
        let years = version
            .records
            .iter()
            .filter(|b| b.date >= first && b.date <= last)
            .map(|b| b.date.year())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        Outcome::Resolved {
            version_ref: VersionRef {
                dataset: Dataset::Daily,
                contract_version: ContractVersion::V1,
                entity: Some(item.entity.clone()),
                version_id: item.base_daily_version_id.clone(),
                integrity_sha256,
            },
            years,
        }
    };
    if crate::sqlite_market::is_latest_only_schema(c)? {
        // Schema 21: the pinned id must name the CURRENT daily series; the
        // year set comes straight from committed `series_bars` days — the
        // same "derived from verified records" surface XP-014 requires.
        let series = match crate::sqlite_market::series::read_series_by_version_id(
            c,
            &item.base_daily_version_id,
        ) {
            Ok(Some(row)) => row,
            Ok(None) => {
                // SPEC §6.1: a superseded (committed-then-replaced) digest is
                // fail-closed integrity, not merely an absent base.
                let stale =
                    crate::sqlite_market::series::decode_version_id(&item.base_daily_version_id)
                        .is_some_and(|d| {
                            crate::sqlite_market::series::digest_seen(c, &d).unwrap_or(false)
                        });
                return Ok(Outcome::Unavailable(if stale {
                    YearPresenceReason::IntegrityFailure
                } else {
                    YearPresenceReason::BaseUnavailable
                }));
            }
            Err(Error::Integrity(_) | Error::Base(_)) => {
                return Ok(Outcome::Unavailable(YearPresenceReason::IntegrityFailure));
            }
            Err(error) => return Err(error),
        };
        if series.entity != item.entity.symbol() || series.period != crate::market_data::Period::Day
        {
            return Ok(Outcome::Unavailable(YearPresenceReason::IntegrityFailure));
        }
        let years = crate::sqlite_market::series::days_in_range(
            c,
            series.series_key,
            crate::compact_market::epoch_day(first),
            crate::compact_market::epoch_day(last),
        )?
        .iter()
        .map(|d| crate::compact_market::epoch_date(*d).map(|date| date.year()))
        .collect::<Result<std::collections::BTreeSet<_>, _>>()
        .map_err(|e| Error::Integrity(e.to_string()))?
        .into_iter()
        .collect::<Vec<_>>();
        return Ok(Outcome::Resolved {
            version_ref: VersionRef {
                dataset: Dataset::Daily,
                contract_version: ContractVersion::V1,
                entity: Some(item.entity.clone()),
                version_id: item.base_daily_version_id.clone(),
                integrity_sha256: series.version_id.clone(),
            },
            years,
        });
    }
    if compact {
        let key =
            match crate::sqlite_market::compact_repo::version_key(c, &item.base_daily_version_id) {
                Ok(key) => key,
                Err(Error::NotFound) => {
                    return Ok(Outcome::Unavailable(YearPresenceReason::BaseUnavailable));
                }
                Err(error) => return Err(error),
            };
        let facts = match crate::stage_trace::timed("presence.item.facts", || ctx.facts(c, key)) {
            Ok(facts) => facts,
            Err(Error::Integrity(_) | Error::Base(_) | Error::NotFound) => {
                return Ok(Outcome::Unavailable(YearPresenceReason::IntegrityFailure));
            }
            Err(error) => return Err(error),
        };
        if facts.state != crate::sqlite_market::compact_versions::CompactVersionState::Published {
            return Ok(Outcome::Unavailable(YearPresenceReason::BaseUnavailable));
        }
        // XP-031 R3: leaf = complete verification, ancestors = committed tier.
        let history = match crate::stage_trace::timed("presence.item.verified", || {
            ctx.presence_history(c, key)
        }) {
            Ok(h) => h,
            Err(Error::Integrity(_) | Error::Base(_) | Error::NotFound) => {
                return Ok(Outcome::Unavailable(YearPresenceReason::IntegrityFailure));
            }
            Err(error) => return Err(error),
        };
        if history.entity != item.entity.symbol()
            || history.period != crate::market_data::Period::Day
        {
            return Ok(Outcome::Unavailable(YearPresenceReason::IntegrityFailure));
        }
        let mut years = std::collections::BTreeSet::new();
        for day in &history.days {
            let Ok(date) = crate::compact_market::epoch_date(*day) else {
                return Ok(Outcome::Unavailable(YearPresenceReason::IntegrityFailure));
            };
            if date >= first && date <= last {
                years.insert(date.year());
            }
        }
        Ok(Outcome::Resolved {
            version_ref: VersionRef {
                dataset: Dataset::Daily,
                contract_version: ContractVersion::V1,
                entity: Some(item.entity.clone()),
                version_id: item.base_daily_version_id.clone(),
                integrity_sha256: crate::compact_market::digest_hex(&history.integrity_digest),
            },
            years: years.into_iter().collect(),
        })
    } else {
        let record = match read_record(c, &item.base_daily_version_id, true, false) {
            Ok(record) => record,
            Err(Error::NotFound) => {
                return Ok(Outcome::Unavailable(YearPresenceReason::BaseUnavailable));
            }
            Err(Error::Integrity(_) | Error::Base(_)) => {
                return Ok(Outcome::Unavailable(YearPresenceReason::IntegrityFailure));
            }
            Err(error) => return Err(error),
        };
        if record.version().symbol != item.entity.symbol() {
            return Ok(Outcome::Unavailable(YearPresenceReason::IntegrityFailure));
        }
        Ok(resolved(
            record.raw()["integrity_sha256"]
                .as_str()
                .ok_or_else(|| Error::Integrity("published record lacks integrity digest".into()))?
                .to_owned(),
            record.version(),
        ))
    }
}

impl SqliteSupply {
    /// Shared body of `year_presence`; `after_item` runs inside the read
    /// transaction after each item resolves (index in plan order). Production
    /// passes a no-op; tests use it to commit from another connection
    /// mid-batch and prove the batch stays on one snapshot.
    pub(crate) fn year_presence_hooked(
        &self,
        plan: &crate::data_supply::presence::PresencePlan,
        mut after_item: impl FnMut(usize),
    ) -> Result<Vec<crate::data_supply::presence::PresenceOutcome>, Error> {
        self.market.read(|c| {
            let compact = crate::sqlite_market::is_compact_schema(c)?;
            let mut ctx = VerifyCtx::default();
            plan.items
                .iter()
                .enumerate()
                .map(|(i, item)| {
                    let outcome = presence_item(c, &mut ctx, compact, item);
                    after_item(i);
                    outcome
                })
                .collect()
        })
    }
}

impl crate::data_supply::presence::PresenceRepository for SqliteSupply {
    /// The whole batch resolves inside ONE `market.read` transaction — a
    /// concurrent commit mid-batch cannot split the snapshot — and shares
    /// this transaction's verified-facts/segment context (XP-011) so
    /// sibling versions reusing immutable ancestors verify once. Items are
    /// processed one at a time; no batch-sized full-history cache.
    fn year_presence(
        &self,
        plan: &crate::data_supply::presence::PresencePlan,
        _access: &Access,
    ) -> Result<Vec<crate::data_supply::presence::PresenceOutcome>, Error> {
        self.year_presence_hooked(plan, |_| {})
    }
}

impl ReadRepository for SqliteSupply {
    fn query(&self, plan: &QueryPlan, access: &Access) -> Result<QuerySnapshot, Error> {
        self.market.read(|c| {
            let mut ctx = VerifyCtx::default();
            query(c, &mut ctx, plan, access)
        })
    }
    fn verify(&self, refs: &[VersionRef]) -> Result<(), Error> {
        self.market.read(|c| {
            let mut ctx = VerifyCtx::default();
            verify_versions_conn(c, &mut ctx, refs)
        })
    }
    fn versions(&self, args: &VersionsQuery, access: &Access) -> Result<Vec<VersionInfo>, Error> {
        let entities = entities(args.dataset, &args.selection, access)?;
        self.market.read(|c|{
            let mut ctx = VerifyCtx::default();
            let mut result=vec![];
            if args.dataset==Dataset::Catalog {
                let mut s=c.prepare("SELECT version_id FROM catalog_versions ORDER BY published_at DESC,version_id ASC").map_err(storage)?;
                let ids=s.query_map([],|r|r.get::<_,String>(0)).map_err(storage)?.collect::<Result<Vec<_>,_>>().map_err(storage)?;
                for id in ids {let v=sqlite_catalog::read(c,&id)?;result.push(VersionInfo{version_ref:catalog_ref(&v)?,published_at:v.published_at});}
            }else{
                let latest=crate::sqlite_market::is_latest_only_schema(c)?;
                let compact=!latest && crate::sqlite_market::is_compact_schema(c)?;
                let orphan:bool=if latest{false}else if compact{c.query_row("SELECT EXISTS(SELECT 1 FROM task_publications tp LEFT JOIN data_versions v ON v.version_key=tp.version_key WHERE v.version_key IS NULL)",[],|r|r.get(0))}else{c.query_row("SELECT EXISTS(SELECT 1 FROM publication_batches b LEFT JOIN data_versions v ON b.version_id=v.version_id WHERE v.version_id IS NULL)",[],|r|r.get(0))}.map_err(storage)?;if orphan{return Err(Error::Integrity("orphan market publication".into()));}
                let list_sql=if compact{"SELECT v.version_id FROM data_versions v WHERE v.entity=?1 AND v.period=?2 AND (v.state=3 OR EXISTS(SELECT 1 FROM task_publications tp WHERE tp.version_key=v.version_key)) ORDER BY v.published_at DESC,v.version_id ASC"}else{"SELECT version_id FROM data_versions v WHERE symbol=?1 AND period=?2 AND (state='published' OR EXISTS(SELECT 1 FROM publication_batches b WHERE b.version_id=v.version_id)) ORDER BY published_at DESC,version_id ASC"};
                for entity in entities {
                    if latest {
                        // SPEC §6.1 list_data_versions on schema 21: at most one
                        // (current) entry per entity.
                        if let Some(row)=crate::sqlite_market::series::read_series(c,&entity.symbol(),if args.dataset==Dataset::Daily{crate::market_data::Period::Day}else{crate::market_data::Period::Year})? {
                            result.push(VersionInfo{version_ref:series_version_ref(args.dataset,&entity,&row),published_at:series_timestamp(&row.updated_at)?});
                        }
                        continue;
                    }
                    let mut s=c.prepare(list_sql).map_err(storage)?;
                    let get_id=|r:&rusqlite::Row<'_>| r.get::<_,String>(0);let ids:Vec<String>=if compact{s.query_map(params![entity.symbol(),if args.dataset==Dataset::Daily{1}else{2}],get_id)}else{s.query_map(params![entity.symbol(),if args.dataset==Dataset::Daily{"day"}else{"year"}],get_id)}.map_err(storage)?.collect::<Result<Vec<_>,_>>().map_err(storage)?;
                    for id in ids {let r=read_market(c,&mut ctx,args.dataset,&entity,&id)?;result.push(VersionInfo{version_ref:market_ref(args.dataset,&entity,&r),published_at:r.version().published_at});}
                }
            }
            result.sort_by(|a,b|b.published_at.cmp(&a.published_at).then(a.version_ref.version_id.cmp(&b.version_ref.version_id)));Ok(result)
        })
    }
}
pub(crate) fn query(
    c: &Connection,
    ctx: &mut VerifyCtx,
    plan: &QueryPlan,
    access: &Access,
) -> Result<QuerySnapshot, Error> {
    let mut result = QuerySnapshot {
        manifest: vec![],
        rows: vec![],
    };
    if plan.args.dataset == Dataset::Catalog {
        let id = match &plan.args.version {
            VersionChoice::CatalogVersion { version_id } => Some(version_id.clone()),
            _ => sqlite_catalog::head(c)?,
        };
        let Some(id) = id else {
            result.manifest.push(ManifestEntry {
                entity: None,
                version_ref: None,
                status: ManifestStatus::Absent,
                metadata: None,
            });
            return Ok(result);
        };
        let v = sqlite_catalog::read(c, &id)?;
        let reference = catalog_ref(&v)?;
        result.manifest.push(ManifestEntry {
            entity: None,
            version_ref: Some(reference.clone()),
            status: ManifestStatus::Present,
            metadata: Some(Metadata {
                source: v.source.clone(),
                published_at: v.published_at,
                coverage_status: "complete".into(),
                coverage_reason: "validated fixture catalog snapshot".into(),
                units: vec![],
                adjustment: None,
                lineage: None,
            }),
        });
        let Filter::Catalog {
            code,
            name_query,
            industry_code,
        } = &plan.args.filter
        else {
            return Err(invalid("catalog filter required"));
        };
        let mut members = v.records;
        members.sort_by_key(|r| r.key());
        for r in members {
            let entity = Security {
                exchange: match r.exchange {
                    crate::catalog::Exchange::Shanghai => Exchange::Sh,
                    crate::catalog::Exchange::Shenzhen => Exchange::Sz,
                    _ => return Err(Error::Integrity("catalog exchange".into())),
                },
                code: r.symbol,
            };
            if !access.allows(Dataset::Catalog, Some(&entity)) {
                continue;
            }
            let industry = match r.industry {
                crate::catalog::IndustryInfo::Assigned { code, name, .. } => {
                    Industry::Assigned { code, name }
                }
                crate::catalog::IndustryInfo::Missing { reason } => Industry::Missing { reason },
                crate::catalog::IndustryInfo::Conflicted { reason } => {
                    Industry::Conflicted { reason }
                }
            };
            if code.as_ref().is_some_and(|v| v != &entity.code)
                || name_query.as_ref().is_some_and(|v| !r.name.contains(v))
                || industry_code
                    .as_ref()
                    .is_some_and(|v| !matches!(&industry,Industry::Assigned{code,..} if code==v))
            {
                continue;
            }
            result.rows.push(Row::Catalog {
                entity,
                version_ref: reference.clone(),
                name: r.name,
                industry,
            });
        }
    } else {
        let latest_only = crate::sqlite_market::is_latest_only_schema(c)?;
        for entity in &plan.entities {
            let id = match &plan.args.version {
                VersionChoice::ExplicitVersionMap { versions } => versions
                    .iter()
                    .find(|v| &v.entity == entity)
                    .map(|v| v.version_id.clone()),
                _ => crate::sqlite_market::head(
                    c,
                    &Scope {
                        symbol: entity.symbol(),
                        period: plan.args.dataset.period().unwrap(),
                    },
                )?,
            };
            let Some(id) = id else {
                result.manifest.push(ManifestEntry {
                    entity: Some(entity.clone()),
                    version_ref: None,
                    status: ManifestStatus::Absent,
                    metadata: None,
                });
                continue;
            };
            if latest_only && plan.args.dataset == Dataset::Yearly {
                // RULINGS A1: a daily `replace` since the recorded base
                // invalidates the entity's stored yearly rows — refuse the
                // row, not the whole request.
                if let Some(series) =
                    crate::sqlite_market::series::read_series_by_version_id(c, &id)?
                    && matches!(
                        crate::sqlite_market::series::yearly_state(c, &series)?,
                        crate::sqlite_market::series::YearlyState::Invalid
                    )
                {
                    result.manifest.push(ManifestEntry {
                        entity: Some(entity.clone()),
                        version_ref: None,
                        status: ManifestStatus::Absent,
                        metadata: None,
                    });
                    continue;
                }
            }
            let r = read_market(c, ctx, plan.args.dataset, entity, &id)?;
            let mut meta_scan = YearlyMetaScan::new();
            let reference = market_ref(plan.args.dataset, entity, &r);
            result.manifest.push(ManifestEntry {
                entity: Some(entity.clone()),
                version_ref: Some(reference.clone()),
                status: ManifestStatus::Present,
                metadata: Some(metadata(&r)),
            });
            let (start, end) = match plan.args.filter {
                Filter::Daily {
                    start_date,
                    end_date,
                } => (start_date, end_date),
                Filter::Yearly {
                    start_year,
                    end_year,
                } => (
                    chrono::NaiveDate::from_ymd_opt(start_year, 1, 1).unwrap(),
                    chrono::NaiveDate::from_ymd_opt(end_year, 12, 31).unwrap(),
                ),
                _ => return Err(invalid("market filter")),
            };
            let range = crate::market_storage::Range {
                start,
                end,
                after: None,
                limit: MAX_ROWS,
            };
            for b in crate::sqlite_market::range_rows(
                c,
                &id,
                plan.args.dataset.period().unwrap(),
                &range,
            )? {
                let (first_trading_date, last_trading_date, year_unfinished) =
                    if plan.args.dataset == Dataset::Yearly {
                        meta_scan.meta(c, &r, b.date)?
                    } else {
                        (None, None, None)
                    };
                result.rows.push(Row::Market {
                    first_trading_date,
                    last_trading_date,
                    year_unfinished,
                    entity: entity.clone(),
                    version_ref: reference.clone(),
                    date: b.date,
                    open: b.open.to_string(),
                    high: b.high.to_string(),
                    low: b.low.to_string(),
                    close: b.close.to_string(),
                    volume: b.volume.map(|v| v.to_string()),
                    amount: b.amount.map(|v| v.to_string()),
                });
                if result.rows.len() > MAX_ROWS {
                    return Err(invalid("snapshot row budget exceeded"));
                }
            }
        }
    }
    Ok(result)
}

/// Availability facts for one selection: the same head resolution, version
/// identity/period check, manifest projection, integrity-failure surface and
/// row budget as the generic `query` path — but one verified materialization
/// instead of two and no row serialization, since availability consumes rows
/// only as dates/count. Yearly rows keep the `yearly_row_meta` lineage
/// re-aggregation check `query` applied.
type AvailabilityStats = (
    Option<ManifestEntry>,
    Option<chrono::NaiveDate>,
    Option<chrono::NaiveDate>,
    usize,
);
// Test-only switch: force the full-verification stats path so tests can
// compare it field by field with the committed-tier fast path.
#[cfg(test)]
thread_local! {
    pub(crate) static FORCE_FULL_STATS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
/// Committed-tier market stats (XP-025 D daily, XP-030 R2 yearly; H10
/// approved "availability per-entity stats/verification at committed
/// tier"): when the query range covers the whole published version, the
/// manifest, metadata and first/last/count come from the version row (its
/// published fields, including the yearly lineage binding, are still
/// verified by `committed_facts`) and its version-evidence row — no chain
/// walk, no node verification, no bar decoding. For yearly versions the
/// lineage is the stored `yearly_lineage` row; the per-bar re-aggregation
/// against the base (`YearlyMetaScan`) is not repeated at this tier.
/// Sub-range filters, unpublished versions and non-compact stores return
/// `None` and keep the fully verified path.
fn committed_market_stats(
    c: &Connection,
    ctx: &mut VerifyCtx,
    plan: &QueryPlan,
    entity: &Security,
    id: &str,
) -> Result<Option<AvailabilityStats>, Error> {
    use crate::sqlite_market::compact_versions::{self, CompactVersionState};
    let key = match crate::sqlite_market::compact_repo::version_key(c, id) {
        Ok(key) => key,
        Err(Error::NotFound) => return Ok(None),
        Err(e) => return Err(e),
    };
    let facts = ctx.committed_facts(c, key)?;
    if facts.state != CompactVersionState::Published {
        return Ok(None);
    }
    let dataset = plan.args.dataset;
    if facts.entity != entity.symbol() || Some(facts.period) != dataset.period() {
        return Err(Error::Integrity("version entity/dataset mismatch".into()));
    }
    let (start, end) = market_range(plan)?;
    if !(crate::compact_market::epoch_day(start) <= facts.first_day
        && facts.last_day <= crate::compact_market::epoch_day(end))
    {
        return Ok(None);
    }
    let count = usize::try_from(facts.record_count)
        .map_err(|_| Error::Integrity("committed record_count out of range".into()))?;
    if count > MAX_ROWS {
        return Err(invalid("snapshot row budget exceeded"));
    }
    let (kind, canonical) = compact_versions::read_evidence(c, &facts.evidence_digest)?;
    if kind != crate::compact_market::EvidenceKind::Version {
        return Err(Error::Integrity("version evidence kind mismatch".into()));
    }
    let evidence: crate::compact_market::VersionEvidence =
        serde_json::from_str(&canonical).map_err(|e| Error::Integrity(e.to_string()))?;
    let published_at = facts
        .published_at
        .as_deref()
        .ok_or_else(|| Error::Integrity("published version lacks published_at".into()))?
        .parse()
        .map_err(|e: chrono::ParseError| Error::Integrity(e.to_string()))?;
    let day =
        |d: i64| crate::compact_market::epoch_date(d).map_err(|e| Error::Integrity(e.to_string()));
    let lineage = if dataset == Dataset::Yearly {
        Some(compact_versions::committed_yearly_lineage(c, key)?)
    } else {
        None
    };
    let manifest = ManifestEntry {
        entity: Some(entity.clone()),
        version_ref: Some(market_ref_facts(dataset, entity, &facts)?),
        status: ManifestStatus::Present,
        metadata: Some(Metadata {
            source: evidence.source,
            published_at,
            coverage_status: match facts.coverage_status {
                1 => "complete",
                2 => "incomplete",
                _ => "unknown",
            }
            .into(),
            coverage_reason: evidence.coverage_reason,
            units: unit_evidence(&evidence.unit_currency),
            adjustment: Some(adjustment_evidence(&evidence.adjustment_evidence)),
            lineage: lineage
                .map(|l| {
                    Ok::<_, Error>(Lineage {
                        base_daily_version_id: l.base_daily_version_id,
                        algorithm_version: l.algorithm_version,
                        first_trading_date: day(l.first_trade_day)?,
                        last_trading_date: day(l.last_trade_day)?,
                    })
                })
                .transpose()?,
        }),
    };
    Ok(Some((
        Some(manifest),
        Some(day(facts.first_day)?),
        Some(day(facts.last_day)?),
        count,
    )))
}
fn availability_stats(
    c: &Connection,
    ctx: &mut VerifyCtx,
    plan: &QueryPlan,
    entity: Option<&Security>,
    access: &Access,
) -> Result<AvailabilityStats, Error> {
    let Some(entity) = entity else {
        // Entity-less selections (catalog) reuse the generic projection.
        let s = query(c, ctx, plan, access)?;
        let dates = s
            .rows
            .iter()
            .filter_map(|r| match r {
                Row::Market { date, .. } => Some(*date),
                _ => None,
            })
            .collect::<Vec<_>>();
        return Ok((
            s.manifest.into_iter().next(),
            dates.first().copied(),
            dates.last().copied(),
            s.rows.len(),
        ));
    };
    let id = match &plan.args.version {
        VersionChoice::ExplicitVersionMap { versions } => versions
            .iter()
            .find(|v| &v.entity == entity)
            .map(|v| v.version_id.clone()),
        _ => crate::sqlite_market::head(
            c,
            &Scope {
                symbol: entity.symbol(),
                period: plan.args.dataset.period().unwrap(),
            },
        )?,
    };
    let Some(id) = id else {
        return Ok((
            Some(ManifestEntry {
                entity: Some(entity.clone()),
                version_ref: None,
                status: ManifestStatus::Absent,
                metadata: None,
            }),
            None,
            None,
            0,
        ));
    };
    if crate::sqlite_market::is_latest_only_schema(c)? {
        return series_stats(c, plan, entity, &id);
    }
    #[cfg(test)]
    let force_full = FORCE_FULL_STATS.with(|f| f.get());
    #[cfg(not(test))]
    let force_full = false;
    if matches!(plan.args.dataset, Dataset::Daily | Dataset::Yearly)
        && !force_full
        && crate::sqlite_market::is_compact_schema(c)?
        && let Some(stats) = committed_market_stats(c, ctx, plan, entity, &id)?
    {
        return Ok(stats);
    }
    // Published compact heads answer the same facts without a full record
    // materialization: the slim projection's manifest/metadata are
    // byte-identical, and the in-window stats come from the windowed leaf
    // read (chain linkage + clip digests still enforced) instead of the
    // materialized `records` list.
    let slim = crate::stage_trace::timed("avail.stats.slim_market", || {
        slim_market(c, ctx, plan.args.dataset, entity, &id)
    })?;
    if let Some(r) = slim {
        let _window = crate::stage_trace::span(if plan.args.dataset == Dataset::Yearly {
            "avail.stats.yearly_window_and_meta"
        } else {
            "avail.stats.daily_window"
        });
        let manifest = ManifestEntry {
            entity: Some(entity.clone()),
            version_ref: Some(market_ref(plan.args.dataset, entity, &r)),
            status: ManifestStatus::Present,
            metadata: Some(metadata(&r)),
        };
        let (start, end) = market_range(plan)?;
        let key = crate::sqlite_market::compact_repo::version_key(c, &id)?;
        let mut count = 0usize;
        let mut first = None;
        let mut last = None;
        let mut meta_scan = YearlyMetaScan::new();
        for b in crate::sqlite_market::compact_versions::leaf_bars_in_range(
            c,
            key,
            crate::compact_market::epoch_day(start),
            crate::compact_market::epoch_day(end),
        )? {
            let date = crate::compact_market::epoch_date(b.trade_day)
                .map_err(|e| Error::Integrity(e.to_string()))?;
            if plan.args.dataset == Dataset::Yearly {
                meta_scan.meta(c, &r, date)?;
            }
            count += 1;
            if count > MAX_ROWS {
                return Err(invalid("snapshot row budget exceeded"));
            }
            if first.is_none() {
                first = Some(date);
            }
            last = Some(date);
        }
        return Ok((Some(manifest), first, last, count));
    }
    let r = read_market(c, ctx, plan.args.dataset, entity, &id)?;
    let manifest = ManifestEntry {
        entity: Some(entity.clone()),
        version_ref: Some(market_ref(plan.args.dataset, entity, &r)),
        status: ManifestStatus::Present,
        metadata: Some(metadata(&r)),
    };
    let (start, end) = market_range(plan)?;
    let mut count = 0usize;
    let mut first = None;
    let mut last = None;
    let mut meta_scan = YearlyMetaScan::new();
    for b in r
        .version()
        .records
        .iter()
        .filter(|b| b.date >= start && b.date <= end)
    {
        if plan.args.dataset == Dataset::Yearly {
            meta_scan.meta(c, &r, b.date)?;
        }
        count += 1;
        if count > MAX_ROWS {
            return Err(invalid("snapshot row budget exceeded"));
        }
        if first.is_none() {
            first = Some(b.date);
        }
        last = Some(b.date);
    }
    Ok((Some(manifest), first, last, count))
}

fn market_range(plan: &QueryPlan) -> Result<(chrono::NaiveDate, chrono::NaiveDate), Error> {
    match plan.args.filter {
        Filter::Daily {
            start_date,
            end_date,
        } => Ok((start_date, end_date)),
        Filter::Yearly {
            start_year,
            end_year,
        } => Ok((
            chrono::NaiveDate::from_ymd_opt(start_year, 1, 1).unwrap(),
            chrono::NaiveDate::from_ymd_opt(end_year, 12, 31).unwrap(),
        )),
        _ => Err(invalid("market filter")),
    }
}

/// Per-year row metadata. v18 reads the stored `yearly_bars` row; compact
/// storage re-aggregates the verified base daily version (the same
/// projection stage-time verification recomputes) and indexes per-year
/// lineage by calendar year.
pub(crate) type YearlyRowMeta = (
    Option<chrono::NaiveDate>,
    Option<chrono::NaiveDate>,
    Option<bool>,
);

pub(crate) fn yearly_row_meta(
    c: &Connection,
    r: &crate::market_storage::compat::CheckedRecord,
    date: chrono::NaiveDate,
) -> Result<YearlyRowMeta, Error> {
    if !crate::sqlite_market::is_compact_schema(c)? {
        let (first, last, finished): (String, String, bool) = c
            .query_row(
                "SELECT first_trade_date,last_trade_date,is_finished FROM yearly_bars \
                 WHERE version_id=?1 AND trade_date=?2",
                params![r.version().version_id, date.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(storage)?;
        return Ok((
            Some(first.parse().map_err(storage)?),
            Some(last.parse().map_err(storage)?),
            Some(!finished),
        ));
    }
    let (lineage, as_of) = yearly_lineage_as_of(c, r)?;
    let base = read_record(c, &lineage.base_daily_version_id, true, false)?;
    let annual = crate::yearly::aggregate(&base.version, None, as_of)
        .map_err(|e| Error::Integrity(format!("yearly re-aggregation: {e}")))?;
    let record = annual
        .records
        .iter()
        .find(|rec| rec.bar.date == date)
        .ok_or_else(|| Error::Integrity("yearly record missing from re-aggregation".into()))?;
    Ok((
        Some(record.lineage.first_trading_date),
        Some(record.lineage.last_trading_date),
        Some(record.year_unfinished),
    ))
}
fn yearly_lineage_as_of<'a>(
    c: &Connection,
    r: &'a crate::market_storage::compat::CheckedRecord,
) -> Result<(&'a crate::market_data::YearlyDerivation, chrono::NaiveDate), Error> {
    let lineage = r
        .version()
        .yearly_derivation
        .as_ref()
        .ok_or_else(|| Error::Integrity("yearly version lacks derivation".into()))?;
    let as_of_day: i64 = c
        .query_row(
            "SELECT l.as_of_day FROM yearly_lineage l \
             JOIN data_versions v ON v.version_key=l.version_key WHERE v.version_id=?1",
            [&r.version().version_id],
            |r| r.get(0),
        )
        .map_err(storage)?;
    let as_of = crate::compact_market::epoch_date(as_of_day)
        .map_err(|e| Error::Integrity(format!("stored yearly as_of: {e}")))?;
    Ok((lineage, as_of))
}
/// Per-version memo for the compact `yearly_row_meta` check: the base daily
/// read + re-aggregation are pure functions of the checked version's stored
/// lineage, so a scan of N yearly rows shares one computation instead of
/// re-materializing the base per row. Keyed by (version_id, as_of_day) —
/// a different record simply recomputes, keeping per-row error order.
pub(crate) struct YearlyMetaScan {
    annual: Option<(String, i64, std::rc::Rc<crate::yearly::AnnualSeries>)>,
}
impl Default for YearlyMetaScan {
    fn default() -> Self {
        Self::new()
    }
}
impl YearlyMetaScan {
    pub(crate) fn new() -> Self {
        Self { annual: None }
    }
    pub(crate) fn meta(
        &mut self,
        c: &Connection,
        r: &crate::market_storage::compat::CheckedRecord,
        date: chrono::NaiveDate,
    ) -> Result<YearlyRowMeta, Error> {
        if crate::sqlite_market::is_latest_only_schema(c)? {
            return crate::sqlite_market::series::yearly_row_meta(c, r, date);
        }
        if !crate::sqlite_market::is_compact_schema(c)? {
            return yearly_row_meta(c, r, date);
        }
        let (lineage, as_of) = yearly_lineage_as_of(c, r)?;
        let as_of_day = crate::compact_market::epoch_day(as_of);
        let hit = self
            .annual
            .as_ref()
            .is_some_and(|(id, day, _)| id == &r.version().version_id && *day == as_of_day);
        if !hit {
            let base = read_record(c, &lineage.base_daily_version_id, true, false)?;
            let annual = crate::yearly::aggregate(&base.version, None, as_of)
                .map_err(|e| Error::Integrity(format!("yearly re-aggregation: {e}")))?;
            self.annual = Some((
                r.version().version_id.clone(),
                as_of_day,
                std::rc::Rc::new(annual),
            ));
        }
        let record = self
            .annual
            .as_ref()
            .unwrap()
            .2
            .records
            .iter()
            .find(|rec| rec.bar.date == date)
            .ok_or_else(|| Error::Integrity("yearly record missing from re-aggregation".into()))?;
        Ok((
            Some(record.lineage.first_trading_date),
            Some(record.lineage.last_trading_date),
            Some(record.year_unfinished),
        ))
    }
}

fn unit_evidence(u: &crate::market_data::UnitCurrencyEvidence) -> Vec<Evidence> {
    [
        ("price_currency", &u.price_currency),
        ("price_unit", &u.price_unit),
        ("volume_unit", &u.volume_unit),
        ("amount_currency", &u.amount_currency),
        ("amount_unit", &u.amount_unit),
    ]
    .into_iter()
    .map(|(field, e)| Evidence {
        field: field.into(),
        status: format!("{:?}", e.status).to_lowercase(),
        value: e.value.clone(),
        reason: e.reason.clone(),
    })
    .collect()
}
fn adjustment_evidence(a: &crate::market_data::AdjustmentEvidence) -> Evidence {
    Evidence {
        field: "price_adjustment".into(),
        status: format!("{:?}", a.status).to_lowercase(),
        value: a.value.clone(),
        reason: a.reason.clone(),
    }
}
fn metadata(r: &crate::market_storage::compat::CheckedRecord) -> Metadata {
    let v = r.version();
    Metadata {
        source: v.source.clone(),
        published_at: v.published_at,
        coverage_status: r.raw()["batch"]["coverage"]["status"]
            .as_str()
            .unwrap_or("Unknown")
            .to_lowercase(),
        coverage_reason: r.raw()["batch"]["coverage"]["reason"]
            .as_str()
            .unwrap_or_default()
            .into(),
        units: unit_evidence(&v.unit_currency),
        adjustment: Some(adjustment_evidence(&v.adjustment_evidence)),
        lineage: v.yearly_derivation.as_ref().map(|l| Lineage {
            base_daily_version_id: l.base_daily_version_id.clone(),
            algorithm_version: l.algorithm_version.clone(),
            first_trading_date: l.first_trading_date,
            last_trading_date: l.last_trading_date,
        }),
    }
}

use crate::data_supply::observe::{
    AvailabilityFacts, ObservationRepository, TaskFacts, authorized,
};
/// The shared task_facts head: owner-only evidence gate, bound-task
/// authorization and request id. Identical for every task_facts variant.
fn task_facts_head(
    c: &Connection,
    id: &str,
    access: &Access,
) -> Result<(String, crate::task_control::TaskSnapshot), Error> {
    // Owner-only check evidence must not leak integrity diagnostics to a reader.
    // This early check can only deny; verified authorization below still applies.
    if *access != Access::local() {
        let private: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM tasks WHERE task_id=?1 AND json_extract(command_key_json,'$.dataset')='task.verification')",
            [id], |r| r.get(0),
        ).map_err(storage)?;
        if private {
            return Err(invalid("forbidden"));
        }
    }
    let s = crate::sqlite_tasks::read_bound_task(c, id)?
        .ok_or(Error::Task(crate::task_control::Error::TaskNotFound))?;
    if !authorized(&s, access) {
        return Err(invalid("forbidden"));
    }
    let request = c
        .query_row(
            "SELECT request_id FROM requests WHERE task_id=?1",
            [id],
            |r| r.get(0),
        )
        .map_err(storage)?;
    Ok((request, s))
}
/// The successful==evidenced binding every output-producing task_facts
/// variant enforces, unchanged across tiers.
fn task_facts_check_evidenced(
    s: &crate::task_control::TaskSnapshot,
    range_results: &[RangeExecutionResult],
    yearly_results: &[YearlyExecutionResult],
) -> Result<(), Error> {
    if matches!(
        s.task.key.operation.as_str(),
        "update_data" | "backfill_data" | "republish_data"
    ) {
        let successful = s
            .items
            .iter()
            .filter(|i| i.outcome.as_ref().is_some_and(|o| o.success))
            .map(|i| i.spec.item_id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let evidenced = if s.task.key.dataset == "market.yearly" {
            yearly_results
                .iter()
                .map(|r| r.item_id.clone())
                .collect::<std::collections::BTreeSet<_>>()
        } else {
            range_results
                .iter()
                .map(|r| r.item_id.clone())
                .collect::<std::collections::BTreeSet<_>>()
        };
        if successful != evidenced {
            return Err(Error::Integrity(
                "successful range task lacks matching result evidence".into(),
            ));
        }
    }
    Ok(())
}
fn task_facts(
    c: &Connection,
    ctx: &mut VerifyCtx,
    id: &str,
    access: &Access,
    verify_outputs: bool,
) -> Result<TaskFacts, Error> {
    let (request, s) = task_facts_head(c, id, access)?;
    if !verify_outputs {
        return Ok(TaskFacts {
            request_id: request,
            snapshot: s,
            publications: vec![],
            range_results: vec![],
            yearly_results: vec![],
        });
    }
    if crate::sqlite_market::is_latest_only_schema(c)? {
        return task_facts_series(c, request, s);
    }
    let mut q = c
        .prepare(if crate::sqlite_market::is_compact_schema(c)?{"SELECT v.version_id FROM task_publications tp JOIN data_versions v ON v.version_key=tp.version_key WHERE tp.task_id=?1 ORDER BY v.version_id"}else{"SELECT version_id FROM task_publications WHERE task_id=?1 ORDER BY version_id"})
        .map_err(storage)?;
    let ids = q
        .query_map([id], |r| r.get::<_, String>(0))
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    let mut publications = vec![];
    for id in ids {
        // Reference-only read: `market_ref` consumes identity + the stored
        // digest, never bars — the header projection skips materialization.
        let r = read_record_slim(c, ctx, &id, true, true)?;
        let e = crate::data_supply::observe::entity(&r.version().symbol)
            .ok_or_else(|| Error::Integrity("published task symbol".into()))?;
        let dataset = if r.version().period == crate::market_data::Period::Day {
            Dataset::Daily
        } else {
            Dataset::Yearly
        };
        publications.push(market_ref(dataset, &e, &r));
    }
    let mut q = c
        .prepare("SELECT result_id FROM market_update_results WHERE task_id=?1 ORDER BY result_id")
        .map_err(storage)?;
    let result_ids = q
        .query_map([id], |r| r.get::<_, String>(0))
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    let range_results = result_ids
        .iter()
        .map(|id| range_result(c, ctx, id))
        .collect::<Result<Vec<_>, _>>()?;
    let mut q = c
        .prepare(if crate::sqlite_market::is_compact_schema(c)? {
            "SELECT publication_id FROM publication_results \
             WHERE task_id=?1 AND publication_kind=2 ORDER BY publication_id"
        } else {
            "SELECT result_id FROM yearly_execution_results WHERE task_id=?1 ORDER BY result_id"
        })
        .map_err(storage)?;
    let yearly_ids = q
        .query_map([id], |r| r.get::<_, String>(0))
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    let yearly_results = yearly_ids
        .iter()
        .map(|id| yearly_result(c, ctx, id))
        .collect::<Result<Vec<_>, _>>()?;
    task_facts_check_evidenced(&s, &range_results, &yearly_results)?;
    Ok(TaskFacts {
        yearly_results,
        range_results,
        request_id: request,
        snapshot: s,
        publications,
    })
}
/// `task_facts(verify_outputs=true)` at the committed-state verification
/// tier (XP-020) — progress read scope only. The authorization head,
/// every publications/range/yearly enumeration query, the
/// successful==evidenced binding and the produced `TaskFacts` are
/// identical; version facts come from `VerifyCtx::committed_facts` —
/// the row plus its recomputed `integrity_digest` — and result rows
/// from the `*_meta_ctx` audit readers instead of full-chain
/// verification, so no bar rows are read. Non-compact databases keep
/// the unchanged full read (`task_facts` is delegated to verbatim).
fn task_facts_committed(
    c: &Connection,
    ctx: &mut VerifyCtx,
    id: &str,
    access: &Access,
) -> Result<TaskFacts, Error> {
    if crate::sqlite_market::is_latest_only_schema(c)? {
        let (request, s) = task_facts_head(c, id, access)?;
        return task_facts_series(c, request, s);
    }
    if !crate::sqlite_market::is_compact_schema(c)? {
        return task_facts(c, ctx, id, access, true);
    }
    let (request, s) = task_facts_head(c, id, access)?;
    let mut q = c
        .prepare("SELECT v.version_id FROM task_publications tp JOIN data_versions v ON v.version_key=tp.version_key WHERE tp.task_id=?1 ORDER BY v.version_id")
        .map_err(storage)?;
    let ids = q
        .query_map([id], |r| r.get::<_, String>(0))
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    let mut publications = vec![];
    for id in ids {
        // Committed tier: the reference consumes identity + stored
        // integrity digest from row facts — no envelope at all.
        let facts = ctx.committed_facts_by_id(c, &id)?;
        let e = crate::data_supply::observe::entity(&facts.entity)
            .ok_or_else(|| Error::Integrity("published task symbol".into()))?;
        let dataset = if facts.period == crate::market_data::Period::Day {
            Dataset::Daily
        } else {
            Dataset::Yearly
        };
        publications.push(market_ref_facts(dataset, &e, &facts)?);
    }
    let mut q = c
        .prepare("SELECT result_id FROM market_update_results WHERE task_id=?1 ORDER BY result_id")
        .map_err(storage)?;
    let result_ids = q
        .query_map([id], |r| r.get::<_, String>(0))
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    let range_results = result_ids
        .iter()
        .map(|id| range_result_meta(c, ctx, id))
        .collect::<Result<Vec<_>, _>>()?;
    let mut q = c
        .prepare(
            "SELECT publication_id FROM publication_results \
             WHERE task_id=?1 AND publication_kind=2 ORDER BY publication_id",
        )
        .map_err(storage)?;
    let yearly_ids = q
        .query_map([id], |r| r.get::<_, String>(0))
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    let yearly_results = yearly_ids
        .iter()
        .map(|id| yearly_result_meta(c, ctx, id))
        .collect::<Result<Vec<_>, _>>()?;
    task_facts_check_evidenced(&s, &range_results, &yearly_results)?;
    Ok(TaskFacts {
        yearly_results,
        range_results,
        request_id: request,
        snapshot: s,
        publications,
    })
}
/// `task_facts(verify_outputs=false)` at summary depth: identical
/// authorization and bound-request validation via `read_bound_task`, but
/// without the publications/range/yearly result reads. `related_tasks`/
/// task-list consumers read only summary fields, dataset keys and item
/// targets; item stage/outcome and scope-occupancy checks still run
/// because skipping them weakened corruption detection (XP-011).
fn task_facts_light(c: &Connection, id: &str, access: &Access) -> Result<TaskFacts, Error> {
    let (request, s) = task_facts_head(c, id, access)?;
    Ok(TaskFacts {
        request_id: request,
        snapshot: s,
        publications: vec![],
        range_results: vec![],
        yearly_results: vec![],
    })
}
/// Whether `access` may see tasks of `dataset` at all.
fn dataset_visible(dataset: &str, access: &Access) -> bool {
    match dataset {
        "task.verification" => *access == Access::local(),
        "market.daily" => access.allows(Dataset::Daily, None),
        "market.yearly" => access.allows(Dataset::Yearly, None),
        "securities.catalog" => access.allows(Dataset::Catalog, None),
        "legacy_market_compound" => {
            access.allows(Dataset::Daily, None) || access.allows(Dataset::Yearly, None)
        }
        _ => false,
    }
}
/// Light facts of ONE task when the caller may see it (dataset allowed and
/// every item symbol allowed); `None` = hidden from this caller. This is
/// the per-row body the old full `task_list` ran for every task.
fn load_task_facts(c: &Connection, id: &str, access: &Access) -> Result<Option<TaskFacts>, Error> {
    let (key, work): (String, String) = c
        .query_row(
            "SELECT command_key_json,frozen_work_json FROM tasks WHERE task_id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(storage)?;
    let key: crate::task_control::CommandKey = serde_json::from_str(&key).map_err(storage)?;
    if !dataset_visible(&key.dataset, access) {
        return Ok(None);
    }
    let work: crate::task_control::FrozenWork = serde_json::from_str(&work).map_err(storage)?;
    if work.items.iter().any(|i| {
        i.target["symbol"].as_str().is_some_and(|s| {
            crate::data_supply::observe::entity(s)
                .is_none_or(|e| access.securities.as_ref().is_some_and(|a| !a.contains(&e)))
        })
    }) {
        return Ok(None);
    }
    match task_facts_light(c, id, access) {
        Ok(f) => Ok(Some(f)),
        Err(Error::Invalid(s)) if s == "forbidden" => Ok(None),
        Err(e) => Err(e),
    }
}
/// Every visible task, newest first. Kept for the trait's full-list
/// accessor; no hard task-count cap (XP-025 D) — bounded callers use
/// `scan_tasks` / `related_tasks_for`.
pub(crate) fn task_list(c: &Connection, access: &Access) -> Result<Vec<TaskFacts>, Error> {
    let mut q = c
        .prepare("SELECT task_id FROM tasks ORDER BY created_at DESC,task_id ASC")
        .map_err(storage)?;
    let ids = q
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    drop(q);
    let mut result = vec![];
    for id in ids {
        if let Some(f) = load_task_facts(c, &id, access)? {
            result.push(f);
        }
    }
    Ok(result)
}
/// One keyset slice of the task list (XP-025 D): SQL does ordering, the
/// snapshot bound and coarse filters; only the rows of this slice are
/// loaded and authorized. `created_at` is stored as `Utc::now().to_rfc3339()`
/// (always `+00:00`, fractional digits only where non-zero), whose
/// lexicographic order is the chronological order.
fn bind(args: &mut Vec<String>, sql: &mut String, clause: &str, value: String) {
    args.push(value);
    sql.push_str(&clause.replace('?', &format!("?{}", args.len())));
}
fn scan_tasks_sql(scan: &crate::data_supply::observe::TaskScan) -> (String, Vec<String>) {
    let mut sql = String::from("SELECT task_id,created_at FROM tasks WHERE 1=1");
    let mut args: Vec<String> = vec![];
    if let Some(upper) = scan.upper {
        bind(
            &mut args,
            &mut sql,
            " AND created_at<=?",
            upper.to_rfc3339(),
        );
    }
    if let Some((at, id)) = &scan.after {
        args.push(at.clone());
        let a = args.len();
        args.push(id.clone());
        let t = args.len();
        sql.push_str(&format!(
            " AND (created_at<?{a} OR (created_at=?{a} AND task_id>?{t}))"
        ));
    }
    if let Some(from) = scan.created_from {
        bind(&mut args, &mut sql, " AND created_at>=?", from.to_rfc3339());
    }
    if let Some(to) = scan.created_to {
        bind(&mut args, &mut sql, " AND created_at<=?", to.to_rfc3339());
    }
    if let Some(dataset) = &scan.dataset {
        bind(
            &mut args,
            &mut sql,
            " AND json_extract(command_key_json,'$.dataset')=?",
            dataset.clone(),
        );
    }
    if let Some(operation) = &scan.operation {
        bind(
            &mut args,
            &mut sql,
            " AND json_extract(command_key_json,'$.operation')=?",
            operation.clone(),
        );
    }
    if let Some(statuses) = &scan.stored_statuses {
        let mut marks = vec![];
        for status in statuses {
            args.push(status.clone());
            marks.push(format!("?{}", args.len()));
        }
        sql.push_str(&format!(" AND status IN ({})", marks.join(",")));
    }
    sql.push_str(&format!(
        " ORDER BY created_at DESC,task_id ASC LIMIT {}",
        scan.batch
    ));
    (sql, args)
}
fn scan_tasks(
    c: &Connection,
    scan: &crate::data_supply::observe::TaskScan,
    access: &Access,
) -> Result<crate::data_supply::observe::TaskScanBatch, Error> {
    let (sql, args) = scan_tasks_sql(scan);
    let mut q = c.prepare(&sql).map_err(storage)?;
    let rows = q
        .query_map(rusqlite::params_from_iter(args.iter()), |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    drop(q);
    let next_after = (rows.len() >= scan.batch)
        .then(|| rows.last().map(|(id, at)| (at.clone(), id.clone())))
        .flatten();
    let mut facts = vec![];
    for (id, _) in rows {
        if let Some(f) = load_task_facts(c, &id, access)? {
            facts.push(f);
        }
    }
    Ok(crate::data_supply::observe::TaskScanBatch { facts, next_after })
}
/// Tasks related to the requested entities (and, for a catalog selection,
/// to the catalog) without scanning every task: ONE narrow-table pass over
/// `task_scopes` (keys `scope_id(symbol, day|year)`, no JSON parsed) joined
/// to `tasks` by primary key, then facts for only the matching tasks.
/// Without a `scope_key` index the scan is O(task_scopes rows).
/// Related tasks of one subject: the selected facts plus the total number of
/// related (dataset-visible) tasks before the in-flight + latest-N cut.
type RelatedSelection = (Vec<std::rc::Rc<TaskFacts>>, usize);
type RelatedTasks = (
    std::collections::HashMap<String, RelatedSelection>,
    RelatedSelection,
);
/// Finished tasks listed per entity besides all in-flight ones (XP-025 D-b).
const RELATED_FINISHED_CAP: usize = 5;
fn related_scopes_sql(keys: usize) -> String {
    let marks = (1..=keys)
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "SELECT s.scope_key,t.created_at,t.task_id,t.status,json_extract(t.command_key_json,'$.dataset') FROM task_scopes s \
         CROSS JOIN tasks t ON t.task_id=s.task_id WHERE s.scope_key IN ({marks})"
    )
}
fn related_tasks_for(
    c: &Connection,
    symbols: &[String],
    catalog: bool,
    access: &Access,
) -> Result<RelatedTasks, Error> {
    use std::collections::{BTreeSet, HashMap};
    // (created_at DESC, task_id ASC, in_flight)
    type Candidate = (std::cmp::Reverse<String>, String, bool);
    let mut key_symbol: HashMap<String, &String> = HashMap::new();
    for symbol in symbols {
        // "daily" is the period gap-verification tasks put in their scope key.
        for period in ["day", "daily", "year"] {
            key_symbol.insert(
                crate::task_control::legacy::scope_id(symbol, period),
                symbol,
            );
        }
    }
    let keys: Vec<&String> = key_symbol.keys().collect();
    let in_flight = |status: &str| crate::sqlite_tasks::ACTIVE_STATUSES.contains(&status);
    let mut by_symbol: HashMap<&String, BTreeSet<Candidate>> = HashMap::new();
    for chunk in keys.chunks(400) {
        let sql = related_scopes_sql(chunk.len());
        let mut q = c.prepare(&sql).map_err(storage)?;
        let rows = q
            .query_map(rusqlite::params_from_iter(chunk.iter()), |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                ))
            })
            .map_err(storage)?;
        for row in rows {
            let (scope_key, created_at, task_id, status, dataset) = row.map_err(storage)?;
            // Datasets the caller cannot see never count toward the total.
            if !dataset
                .as_deref()
                .is_some_and(|d| dataset_visible(d, access))
            {
                continue;
            }
            if let Some(symbol) = key_symbol.get(&scope_key) {
                by_symbol.entry(symbol).or_default().insert((
                    std::cmp::Reverse(created_at),
                    task_id,
                    in_flight(&status),
                ));
            }
        }
    }
    let mut catalog_ids: BTreeSet<Candidate> = BTreeSet::new();
    if catalog && dataset_visible("securities.catalog", access) {
        let mut q = c
            .prepare(
                "SELECT created_at,task_id,status FROM tasks \
                 WHERE json_extract(command_key_json,'$.dataset')='securities.catalog'",
            )
            .map_err(storage)?;
        let rows = q
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(storage)?;
        for row in rows {
            let (created_at, task_id, status) = row.map_err(storage)?;
            catalog_ids.insert((std::cmp::Reverse(created_at), task_id, in_flight(&status)));
        }
    }
    // All in-flight tasks plus the newest RELATED_FINISHED_CAP finished ones,
    // in list order; facts are loaded once per distinct task.
    let mut loaded: HashMap<String, Option<std::rc::Rc<TaskFacts>>> = HashMap::new();
    let mut select = |candidates: &BTreeSet<Candidate>| -> Result<RelatedSelection, Error> {
        let mut finished = 0usize;
        let mut tasks = vec![];
        for (_, id, active) in candidates {
            if !active {
                if finished >= RELATED_FINISHED_CAP {
                    continue;
                }
                finished += 1;
            }
            let facts = match loaded.get(id) {
                Some(found) => found.clone(),
                None => {
                    let facts = load_task_facts(c, id, access)?.map(std::rc::Rc::new);
                    loaded.insert(id.clone(), facts.clone());
                    facts
                }
            };
            if let Some(f) = facts {
                tasks.push(f);
            }
        }
        Ok((tasks, candidates.len()))
    };
    let mut related = HashMap::new();
    for (symbol, candidates) in &by_symbol {
        related.insert((*symbol).clone(), select(candidates)?);
    }
    let catalog_related = select(&catalog_ids)?;
    Ok((related, catalog_related))
}
/// Connection-level version-identity comparison shared by
/// `ReadRepository::verify` and `SqliteProgressRead::verify_versions`:
/// the `unique_refs`/`market_ref`/`catalog_ref` loop, unchanged.
fn verify_versions_conn(
    c: &Connection,
    ctx: &mut VerifyCtx,
    refs: &[VersionRef],
) -> Result<(), Error> {
    for r in unique_refs(refs) {
        let actual = if r.dataset == Dataset::Catalog {
            catalog_ref(&sqlite_catalog::read(c, &r.version_id)?)?
        } else {
            let entity = r
                .entity
                .as_ref()
                .ok_or_else(|| invalid("missing version entity"))?;
            market_ref(
                r.dataset,
                entity,
                &read_market(c, ctx, r.dataset, entity, &r.version_id)?,
            )
        };
        if actual != r {
            return Err(Error::Integrity("fixed version changed".into()));
        }
    }
    Ok(())
}
/// Connection-level expected == actual comparisons for range/yearly
/// results — the same loops the repository methods ran in their own
/// read transactions, now reusable inside a shared progress scope.
fn verify_range_results_conn(
    c: &Connection,
    ctx: &mut VerifyCtx,
    results: &[RangeExecutionResult],
) -> Result<(), Error> {
    for expected in results {
        if range_result(c, ctx, &expected.result_id)? != *expected {
            return Err(Error::Integrity(
                "range result changed since snapshot".into(),
            ));
        }
    }
    Ok(())
}
fn verify_yearly_results_conn(
    c: &Connection,
    ctx: &mut VerifyCtx,
    results: &[YearlyExecutionResult],
) -> Result<(), Error> {
    for expected in results {
        if yearly_result(c, ctx, &expected.result_id)? != *expected {
            return Err(Error::Integrity(
                "yearly result changed since snapshot".into(),
            ));
        }
    }
    Ok(())
}
/// `verify_versions_conn` at the committed-state tier (XP-020) —
/// progress read scope only. The same `unique_refs` loop and the same
/// expected == actual `VersionRef` comparison; market refs are built
/// from `committed_facts` (entity/period match enforced) instead of
/// full-verification record reads. Catalog refs are unchanged.
/// Non-compact databases delegate to `verify_versions_conn` verbatim.
fn verify_versions_meta_conn(
    c: &Connection,
    ctx: &mut VerifyCtx,
    refs: &[VersionRef],
) -> Result<(), Error> {
    if crate::sqlite_market::is_latest_only_schema(c)? {
        // Committed-tier verify on schema 21: the ref's digest-derived id
        // must still name the current series row (stale pins are a hard
        // `stale_version`, not a silent compare failure) and the stored
        // integrity hex equals the id by construction (SPEC §2.3).
        for r in unique_refs(refs) {
            let actual = if r.dataset == Dataset::Catalog {
                catalog_ref(&sqlite_catalog::read(c, &r.version_id)?)?
            } else {
                let entity = r
                    .entity
                    .as_ref()
                    .ok_or_else(|| invalid("missing version entity"))?;
                let row = crate::sqlite_market::series::require_series(c, &r.version_id)?;
                if row.entity != entity.symbol() || Some(row.period) != r.dataset.period() {
                    return Err(Error::Integrity("version entity/dataset mismatch".into()));
                }
                series_version_ref(r.dataset, entity, &row)
            };
            if actual != r {
                return Err(Error::Integrity("fixed version changed".into()));
            }
        }
        return Ok(());
    }
    if !crate::sqlite_market::is_compact_schema(c)? {
        return verify_versions_conn(c, ctx, refs);
    }
    for r in unique_refs(refs) {
        let actual = if r.dataset == Dataset::Catalog {
            catalog_ref(&sqlite_catalog::read(c, &r.version_id)?)?
        } else {
            let entity = r
                .entity
                .as_ref()
                .ok_or_else(|| invalid("missing version entity"))?;
            let facts = ctx.committed_facts_by_id(c, &r.version_id)?;
            if facts.entity != entity.symbol() || Some(facts.period) != r.dataset.period() {
                return Err(Error::Integrity("version entity/dataset mismatch".into()));
            }
            market_ref_facts(r.dataset, entity, &facts)?
        };
        if actual != r {
            return Err(Error::Integrity("fixed version changed".into()));
        }
    }
    Ok(())
}
/// `verify_range_results_conn` at the committed-state tier (XP-020):
/// the same re-derive-and-compare loop over the committed-tier
/// `range_result_meta`. Committed-facts memoization inside `ctx` makes
/// the second pass per-version O(1); the comparison itself still runs.
/// Non-compact databases delegate to `verify_range_results_conn`.
fn verify_range_results_meta_conn(
    c: &Connection,
    ctx: &mut VerifyCtx,
    results: &[RangeExecutionResult],
) -> Result<(), Error> {
    if !crate::sqlite_market::is_compact_schema(c)? {
        return verify_range_results_conn(c, ctx, results);
    }
    for expected in results {
        if range_result_meta(c, ctx, &expected.result_id)? != *expected {
            return Err(Error::Integrity(
                "range result changed since snapshot".into(),
            ));
        }
    }
    Ok(())
}
/// `verify_yearly_results_conn` at the committed-state tier (XP-020):
/// same contract as `verify_range_results_meta_conn`.
fn verify_yearly_results_meta_conn(
    c: &Connection,
    ctx: &mut VerifyCtx,
    results: &[YearlyExecutionResult],
) -> Result<(), Error> {
    if !crate::sqlite_market::is_compact_schema(c)? {
        return verify_yearly_results_conn(c, ctx, results);
    }
    for expected in results {
        if yearly_result_meta(c, ctx, &expected.result_id)? != *expected {
            return Err(Error::Integrity(
                "yearly result changed since snapshot".into(),
            ));
        }
    }
    Ok(())
}
/// Receipt comparisons on an already-open connection — the same
/// `(_, receipt)` mapping both backend `check_receipt` implementations
/// apply to `publication_checks::read_receipt` /
/// `sqlite_verification::read_receipt`.
fn verify_publication_check_conn(
    c: &Connection,
    receipt: &Option<crate::publication_verification::decision::Receipt>,
) -> Result<(), Error> {
    if let Some(expected) = receipt {
        let actual = crate::sqlite_market::publication_checks::read_receipt(
            c,
            &expected.verification_task_id,
        )?
        .map(|(_, receipt)| receipt);
        if actual.as_ref() != Some(expected) {
            return Err(Error::Integrity(
                "publication check changed since snapshot".into(),
            ));
        }
    }
    Ok(())
}
fn verify_stopped_check_conn(
    c: &Connection,
    receipt: &Option<crate::task_verification::decision::Receipt>,
) -> Result<(), Error> {
    if let Some(expected) = receipt {
        let actual = crate::sqlite_verification::read_receipt(c, &expected.verification_task_id)?
            .map(|(_, r)| r);
        if actual.as_ref() != Some(expected) {
            return Err(Error::Integrity(
                "stopped check changed since snapshot".into(),
            ));
        }
    }
    Ok(())
}

/// `ProgressRead` bound to one SQLite read transaction: every method
/// runs on `c` through the existing connection-level helpers and shares
/// `ctx`, so verified work is reused *inside this snapshot only* — the
/// reader, its `VerifyCtx` and its certificates die with the
/// `market.read` scope (XP-016). No globals, no TLS, no nested
/// `market.read` inside the six methods.
struct SqliteProgressRead<'a> {
    c: &'a Connection,
    ctx: std::cell::RefCell<VerifyCtx>,
}
impl crate::data_supply::observe::ProgressRead for SqliteProgressRead<'_> {
    /// Progress/metadata reads run at the committed-state verification
    /// tier (XP-020): every touched `data_versions` row is re-verified
    /// by `committed_facts` (published state + recomputed
    /// `integrity_digest`, including the parent/lineage reads
    /// `build_identity` requires), every result row is digest-checked
    /// and delta clips are checked at `bar_segments` header level — no
    /// bar rows, no chain reconstruction. All binding, authorization,
    /// cursor and comparison checks are unchanged. The full
    /// verification tier still backs `fixed`/`range`/history reads and
    /// every non-progress caller of the original helpers.
    fn task(&self, id: &str, access: &Access) -> Result<TaskFacts, Error> {
        task_facts_committed(self.c, &mut self.ctx.borrow_mut(), id, access)
    }
    fn verify_versions(&self, refs: &[VersionRef]) -> Result<(), Error> {
        verify_versions_meta_conn(self.c, &mut self.ctx.borrow_mut(), refs)
    }
    fn verify_range_results(&self, results: &[RangeExecutionResult]) -> Result<(), Error> {
        if results.is_empty() {
            return Ok(());
        }
        verify_range_results_meta_conn(self.c, &mut self.ctx.borrow_mut(), results)
    }
    fn verify_yearly_results(&self, results: &[YearlyExecutionResult]) -> Result<(), Error> {
        if results.is_empty() {
            return Ok(());
        }
        verify_yearly_results_meta_conn(self.c, &mut self.ctx.borrow_mut(), results)
    }
    fn verify_publication_check(
        &self,
        receipt: &Option<crate::publication_verification::decision::Receipt>,
    ) -> Result<(), Error> {
        verify_publication_check_conn(self.c, receipt)
    }
    fn verify_stopped_check(
        &self,
        receipt: &Option<crate::task_verification::decision::Receipt>,
    ) -> Result<(), Error> {
        verify_stopped_check_conn(self.c, receipt)
    }
}

impl ObservationRepository for SqliteSupply {
    /// One progress RPC = one `market.read` transaction + one
    /// `VerifyCtx`: `task` and all `verify_*` phases execute inside the
    /// same SQLite snapshot, reusing verified work across phases.
    fn with_progress_read<T>(
        &self,
        f: impl FnOnce(&dyn crate::data_supply::observe::ProgressRead) -> Result<T, Error>,
    ) -> Result<T, Error> {
        self.market.read(|c| {
            let reader = SqliteProgressRead {
                c,
                ctx: std::cell::RefCell::new(VerifyCtx::default()),
            };
            f(&reader)
        })
    }
    fn tasks(&self, access: &Access) -> Result<Vec<TaskFacts>, Error> {
        self.market.read(|c| task_list(c, access))
    }
    fn tasks_scan(
        &self,
        scan: &crate::data_supply::observe::TaskScan,
        access: &Access,
    ) -> Result<crate::data_supply::observe::TaskScanBatch, Error> {
        self.market.read(|c| scan_tasks(c, scan, access))
    }
    fn task(&self, id: &str, access: &Access) -> Result<TaskFacts, Error> {
        self.market.read(|c| {
            let mut ctx = VerifyCtx::default();
            task_facts(c, &mut ctx, id, access, true)
        })
    }
    fn verify_versions(&self, refs: &[VersionRef]) -> Result<(), Error> {
        ReadRepository::verify(self, refs)
    }
    fn verify_versions_committed(&self, refs: &[VersionRef]) -> Result<(), Error> {
        if refs.is_empty() {
            return Ok(());
        }
        self.market
            .read(|c| verify_versions_meta_conn(c, &mut VerifyCtx::default(), refs))
    }
    fn verify_publication_check(
        &self,
        receipt: &Option<crate::publication_verification::decision::Receipt>,
    ) -> Result<(), Error> {
        if receipt.is_none() {
            return Ok(());
        }
        self.market
            .read(|c| verify_publication_check_conn(c, receipt))
    }
    fn verify_stopped_check(
        &self,
        receipt: &Option<crate::task_verification::decision::Receipt>,
    ) -> Result<(), Error> {
        if receipt.is_none() {
            return Ok(());
        }
        self.market.read(|c| verify_stopped_check_conn(c, receipt))
    }
    fn verify_yearly_results(&self, results: &[YearlyExecutionResult]) -> Result<(), Error> {
        if results.is_empty() {
            return Ok(());
        }
        self.market.read(|c| {
            let mut ctx = VerifyCtx::default();
            verify_yearly_results_conn(c, &mut ctx, results)
        })
    }
    fn verify_range_results(&self, results: &[RangeExecutionResult]) -> Result<(), Error> {
        if results.is_empty() {
            return Ok(());
        }
        self.market.read(|c| {
            let mut ctx = VerifyCtx::default();
            verify_range_results_conn(c, &mut ctx, results)
        })
    }
    fn availability(
        &self,
        plan: &QueryPlan,
        access: &Access,
    ) -> Result<Vec<AvailabilityFacts>, Error> {
        let _total = crate::stage_trace::span("availability.total");
        self.market.read(|c| {
            let mut ctx = VerifyCtx::default();
            let entities: Vec<Option<Security>> = if plan.args.dataset == Dataset::Catalog {
                vec![None]
            } else {
                plan.entities.iter().cloned().map(Some).collect()
            };
            // Related tasks come from the requested entities only (XP-025 D):
            // one narrow task_scopes pass, facts for the matching tasks.
            let task_span = crate::stage_trace::span("availability.related_tasks");
            let symbols: Vec<String> = entities.iter().flatten().map(|e| e.symbol()).collect();
            let (related, catalog_related) =
                related_tasks_for(c, &symbols, entities.iter().any(Option::is_none), access)?;
            crate::stage_trace::note(
                "availability.related_tasks_size",
                &[(
                    "tasks",
                    related.values().map(|(t, _)| t.len()).sum::<usize>() as u64
                        + catalog_related.0.len() as u64,
                )],
            );
            drop(task_span);
            let mut out = vec![];
            for entity in entities {
                let (related_tasks, related_tasks_total) = match &entity {
                    Some(e) => related.get(&e.symbol()).cloned().unwrap_or_default(),
                    None => catalog_related.clone(),
                };
                let proven_gaps = match (&entity, plan.args.dataset.period()) {
                    (Some(e), Some(period)) => {
                        crate::sqlite_market::gaps::read_open_gaps(c, &e.symbol(), period)?
                    }
                    _ => vec![],
                };
                match availability_stats(c, &mut ctx, plan, entity.as_ref(), access) {
                    Ok((manifest, first_record, last_record, record_count)) => {
                        out.push(AvailabilityFacts {
                            entity,
                            manifest,
                            first_record,
                            last_record,
                            record_count,
                            integrity_failure: None,
                            proven_gaps,
                            related_tasks,
                            related_tasks_total,
                        });
                    }
                    Err(Error::Integrity(reason) | Error::Base(reason)) => {
                        out.push(AvailabilityFacts {
                            entity,
                            manifest: None,
                            first_record: None,
                            last_record: None,
                            record_count: 0,
                            integrity_failure: Some(reason),
                            proven_gaps,
                            related_tasks,
                            related_tasks_total,
                        })
                    }
                    Err(e) => return Err(e),
                }
            }
            Ok(out)
        })
    }
}

fn range_result(
    c: &Connection,
    ctx: &mut VerifyCtx,
    id: &str,
) -> Result<RangeExecutionResult, Error> {
    if crate::sqlite_market::is_latest_only_schema(c)? {
        return range_result_series(c, id);
    }
    let (audit, receipt) = crate::sqlite_market::update::read_result_ctx(c, ctx, id)?
        .ok_or_else(|| Error::Integrity("range result missing".into()))?;
    let base_id = audit["evidence"]["base_version_id"]
        .as_str()
        .ok_or_else(|| Error::Integrity("range base reference missing".into()))?;
    // These reads share the task/receipt transaction; never query a latest head.
    let base = read_record_slim(c, ctx, base_id, true, true)?;
    let result = read_record_slim(c, ctx, &receipt.version_id, true, true)?;
    if base.version().period != crate::market_data::Period::Day
        || result.version().period != crate::market_data::Period::Day
        || base.version().symbol != result.version().symbol
    {
        return Err(Error::Integrity(
            "range result version identity/period mismatch".into(),
        ));
    }
    let entity = crate::data_supply::observe::entity(&base.version().symbol)
        .ok_or_else(|| Error::Integrity("range result entity missing".into()))?;
    serde_json::from_value(serde_json::json!({
        "result_id":id,"item_id":audit["command"]["item_id"],
        "base_version":market_ref(Dataset::Daily,&entity,&base),
        "result_version":market_ref(Dataset::Daily,&entity,&result),
        "no_change":audit["no_change"],"acquired_at":audit["segment"]["provenance"]["acquired_at"]
    }))
    .map_err(|e| Error::Integrity(format!("range result projection: {e}")))
}

fn yearly_result(
    c: &Connection,
    ctx: &mut VerifyCtx,
    id: &str,
) -> Result<YearlyExecutionResult, Error> {
    if crate::sqlite_market::is_latest_only_schema(c)? {
        return yearly_result_series(c, id);
    }
    let (audit, receipt) = crate::sqlite_market::yearly_results::read_result_ctx(c, ctx, id)?
        .ok_or_else(|| Error::Integrity("yearly result missing".into()))?;
    let base_id = audit["base_daily_version_id"]
        .as_str()
        .ok_or_else(|| Error::Integrity("yearly base missing".into()))?;
    let base = read_record_slim(c, ctx, base_id, true, true)?;
    let result = read_record_slim(c, ctx, &receipt.version_id, true, true)?;
    if base.version().period != crate::market_data::Period::Day
        || result.version().period != crate::market_data::Period::Year
        || base.version().symbol != result.version().symbol
    {
        return Err(Error::Integrity("yearly result identity/period".into()));
    }
    let entity = crate::data_supply::observe::entity(&base.version().symbol)
        .ok_or_else(|| Error::Integrity("yearly result entity".into()))?;
    serde_json::from_value(serde_json::json!({"result_id":id,"item_id":audit["command"]["item_id"],"base_daily_version":market_ref(Dataset::Daily,&entity,&base),"result_version":market_ref(Dataset::Yearly,&entity,&result),"no_change":audit["no_change"],"derived_at":audit["candidate"]["batch"]["provenance"]["acquired_at"]})).map_err(|e|Error::Integrity(e.to_string()))
}

/// `range_result` at the committed-state verification tier (XP-020) —
/// progress read scope only. The audit comes from
/// `update::read_result_meta_ctx`; base/result identity, period and
/// entity checks run on committed facts and the `VersionRef`s are
/// built from the same stored digests — producing a
/// `RangeExecutionResult` identical to `range_result`. Non-compact
/// databases delegate to `range_result` verbatim.
fn range_result_meta(
    c: &Connection,
    ctx: &mut VerifyCtx,
    id: &str,
) -> Result<RangeExecutionResult, Error> {
    if crate::sqlite_market::is_latest_only_schema(c)? {
        return range_result_series(c, id);
    }
    if !crate::sqlite_market::is_compact_schema(c)? {
        return range_result(c, ctx, id);
    }
    let (audit, receipt) = crate::sqlite_market::update::read_result_meta_ctx(c, ctx, id)?
        .ok_or_else(|| Error::Integrity("range result missing".into()))?;
    let base_id = audit["evidence"]["base_version_id"]
        .as_str()
        .ok_or_else(|| Error::Integrity("range base reference missing".into()))?;
    // These reads share the task/receipt transaction; never query a latest head.
    let base = ctx.committed_facts_by_id(c, base_id)?;
    let result = ctx.committed_facts_by_id(c, &receipt.version_id)?;
    if base.period != crate::market_data::Period::Day
        || result.period != crate::market_data::Period::Day
        || base.entity != result.entity
    {
        return Err(Error::Integrity(
            "range result version identity/period mismatch".into(),
        ));
    }
    let entity = crate::data_supply::observe::entity(&base.entity)
        .ok_or_else(|| Error::Integrity("range result entity missing".into()))?;
    let base_ref = market_ref_facts(Dataset::Daily, &entity, &base)?;
    let result_ref = market_ref_facts(Dataset::Daily, &entity, &result)?;
    serde_json::from_value(serde_json::json!({
        "result_id":id,"item_id":audit["command"]["item_id"],
        "base_version":base_ref,"result_version":result_ref,
        "no_change":audit["no_change"],"acquired_at":audit["segment"]["provenance"]["acquired_at"]
    }))
    .map_err(|e| Error::Integrity(format!("range result projection: {e}")))
}

/// `yearly_result` at the committed-state verification tier (XP-020) —
/// progress read scope only; same contract as `range_result_meta`.
fn yearly_result_meta(
    c: &Connection,
    ctx: &mut VerifyCtx,
    id: &str,
) -> Result<YearlyExecutionResult, Error> {
    if crate::sqlite_market::is_latest_only_schema(c)? {
        return yearly_result_series(c, id);
    }
    if !crate::sqlite_market::is_compact_schema(c)? {
        return yearly_result(c, ctx, id);
    }
    let (audit, receipt) = crate::sqlite_market::yearly_results::read_result_meta_ctx(c, ctx, id)?
        .ok_or_else(|| Error::Integrity("yearly result missing".into()))?;
    let base_id = audit["base_daily_version_id"]
        .as_str()
        .ok_or_else(|| Error::Integrity("yearly base missing".into()))?;
    let base = ctx.committed_facts_by_id(c, base_id)?;
    let result = ctx.committed_facts_by_id(c, &receipt.version_id)?;
    if base.period != crate::market_data::Period::Day
        || result.period != crate::market_data::Period::Year
        || base.entity != result.entity
    {
        return Err(Error::Integrity("yearly result identity/period".into()));
    }
    let entity = crate::data_supply::observe::entity(&base.entity)
        .ok_or_else(|| Error::Integrity("yearly result entity".into()))?;
    let base_ref = market_ref_facts(Dataset::Daily, &entity, &base)?;
    let result_ref = market_ref_facts(Dataset::Yearly, &entity, &result)?;
    serde_json::from_value(serde_json::json!({"result_id":id,"item_id":audit["command"]["item_id"],"base_daily_version":base_ref,"result_version":result_ref,"no_change":audit["no_change"],"derived_at":audit["candidate"]["batch"]["provenance"]["acquired_at"]})).map_err(|e|Error::Integrity(e.to_string()))
}

// ---------------------------------------------------------------------------
// Schema-21 latest-only helpers
// ---------------------------------------------------------------------------

fn series_timestamp(text: &str) -> Result<DateTime<Utc>, Error> {
    DateTime::parse_from_rfc3339(text)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|e| Error::Integrity(format!("series timestamp: {e}")))
}

/// `version_id` IS `lower(hex(content_digest))` on schema 21, so the pinned
/// integrity digest equals the id (SPEC §2.3/§6.1).
fn series_version_ref(
    dataset: Dataset,
    entity: &Security,
    series: &crate::sqlite_market::series::SeriesRow,
) -> VersionRef {
    VersionRef {
        dataset,
        contract_version: ContractVersion::V1,
        entity: Some(entity.clone()),
        version_id: series.version_id.clone(),
        integrity_sha256: series.version_id.clone(),
    }
}

/// Wire `Metadata` for a series row: evidence fields from the verified
/// `version` evidence bundle, coverage from the stored state, lineage from
/// `yearly_series_lineage` (period=2 only).
fn series_metadata(
    c: &Connection,
    series: &crate::sqlite_market::series::SeriesRow,
) -> Result<Metadata, Error> {
    let (kind, canonical) =
        crate::sqlite_market::compact_versions::read_evidence(c, &series.evidence_digest)?;
    if kind != crate::compact_market::EvidenceKind::Version {
        return Err(Error::Integrity("version evidence kind mismatch".into()));
    }
    let evidence: crate::compact_market::VersionEvidence =
        serde_json::from_str(&canonical).map_err(|e| Error::Integrity(e.to_string()))?;
    let lineage = if series.period == crate::market_data::Period::Year {
        crate::sqlite_market::series::yearly_lineage(c, series.series_key)?
            .map(|l| {
                Ok::<_, Error>(Lineage {
                    base_daily_version_id: crate::sqlite_market::series::version_id(
                        &l.base_daily_digest,
                    ),
                    algorithm_version: l.algorithm_version.clone(),
                    first_trading_date: crate::compact_market::epoch_date(l.first_trade_day)
                        .map_err(|e| Error::Integrity(e.to_string()))?,
                    last_trading_date: crate::compact_market::epoch_date(l.last_trade_day)
                        .map_err(|e| Error::Integrity(e.to_string()))?,
                })
            })
            .transpose()?
    } else {
        None
    };
    Ok(Metadata {
        source: evidence.source,
        published_at: series_timestamp(&series.updated_at)?,
        coverage_status: match series.coverage_status {
            crate::market_data::CoverageStatus::Complete => "complete".into(),
            crate::market_data::CoverageStatus::Incomplete => "incomplete".into(),
            crate::market_data::CoverageStatus::Unknown => "unknown".into(),
        },
        coverage_reason: evidence.coverage_reason,
        units: unit_evidence(&evidence.unit_currency),
        adjustment: Some(adjustment_evidence(&evidence.adjustment_evidence)),
        lineage,
    })
}

/// `availability` facts for schema 21: the window COUNT/MIN/MAX rides the
/// `series_bars` PK; a yearly row invalidated by a daily `replace` reports
/// `integrity_failure` for that entity (RULINGS A1), stale explicit pins
/// propagate `stale_version`.
fn series_stats(
    c: &Connection,
    plan: &QueryPlan,
    entity: &Security,
    id: &str,
) -> Result<AvailabilityStats, Error> {
    let series = crate::sqlite_market::series::require_series(c, id)?;
    if series.entity != entity.symbol() || Some(series.period) != plan.args.dataset.period() {
        return Err(Error::Integrity("version entity/dataset mismatch".into()));
    }
    if plan.args.dataset == Dataset::Yearly
        && matches!(
            crate::sqlite_market::series::yearly_state(c, &series)?,
            crate::sqlite_market::series::YearlyState::Invalid
        )
    {
        return Err(Error::Integrity(
            "yearly lineage invalidated by replace event".into(),
        ));
    }
    let (start, end) = market_range(plan)?;
    let (count, first, last) = crate::sqlite_market::series::window_stats(
        c,
        series.series_key,
        crate::compact_market::epoch_day(start),
        crate::compact_market::epoch_day(end),
    )?;
    if count > MAX_ROWS {
        return Err(invalid("snapshot row budget exceeded"));
    }
    let manifest = ManifestEntry {
        entity: Some(entity.clone()),
        version_ref: Some(series_version_ref(plan.args.dataset, entity, &series)),
        status: ManifestStatus::Present,
        metadata: Some(series_metadata(c, &series)?),
    };
    let to_date =
        |d: i64| crate::compact_market::epoch_date(d).map_err(|e| Error::Integrity(e.to_string()));
    Ok((
        Some(manifest),
        first.map(to_date).transpose()?,
        last.map(to_date).transpose()?,
        count,
    ))
}

/// `task_facts` at event-era granularity (SPEC §6.1): `market_events` rows
/// ARE the result/publication records. `acquired_at`/`derived_at`
/// approximate to the event commit stamp — the v19 audit-JSON detail is not
/// retained on schema 21 — while the per-item contract binding
/// (`task_facts_check_evidenced`) runs unchanged.
fn task_facts_series(
    c: &Connection,
    request: String,
    s: crate::task_control::TaskSnapshot,
) -> Result<TaskFacts, Error> {
    type Row = (
        String,
        i64,
        String,
        i64,
        String,
        String,
        Option<Vec<u8>>,
        Vec<u8>,
        Option<Vec<u8>>,
        String,
    );
    let events: Vec<Row> = c
        .prepare(
            "SELECT entity,period,kind,outcome,item_id,result_id,old_content_digest,new_content_digest,base_content_digest,created_at FROM market_events WHERE task_id=?1 ORDER BY seq",
        )
        .map_err(storage)?
        .query_map([s.task.task_id.as_str()], |r| {
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
                r.get(9)?,
            ))
        })
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    let digest_hex = |raw: &[u8]| -> Result<String, Error> {
        Ok(crate::compact_market::digest_hex(
            &<[u8; 32]>::try_from(raw)
                .map_err(|_| Error::Integrity("event digest malformed".into()))?,
        ))
    };
    let mut publications = Vec::new();
    let mut range_results = Vec::new();
    let mut yearly_results = Vec::new();
    for (entity, period, kind, outcome, item_id, result_id, old, new, base, created) in events {
        let e = crate::data_supply::observe::entity(&entity)
            .ok_or_else(|| Error::Integrity("event entity is not a security".into()))?;
        let new_id = digest_hex(&new)?;
        let when = series_timestamp(&created)?;
        if outcome == 2 {
            publications.push(VersionRef {
                dataset: if period == 1 {
                    Dataset::Daily
                } else {
                    Dataset::Yearly
                },
                contract_version: ContractVersion::V1,
                entity: Some(e.clone()),
                version_id: new_id.clone(),
                integrity_sha256: new_id.clone(),
            });
        }
        if period == 1 && kind != "first_publish" {
            let base_id =
                digest_hex(&old.ok_or_else(|| Error::Integrity("event lacks old digest".into()))?)?;
            range_results.push(RangeExecutionResult {
                result_id,
                item_id,
                base_version: VersionRef {
                    dataset: Dataset::Daily,
                    contract_version: ContractVersion::V1,
                    entity: Some(e.clone()),
                    version_id: base_id.clone(),
                    integrity_sha256: base_id,
                },
                result_version: VersionRef {
                    dataset: Dataset::Daily,
                    contract_version: ContractVersion::V1,
                    entity: Some(e),
                    version_id: new_id.clone(),
                    integrity_sha256: new_id,
                },
                no_change: outcome == 1,
                acquired_at: when,
            });
        } else if period == 2 {
            let base_id = digest_hex(
                &base.ok_or_else(|| Error::Integrity("yearly event lacks base digest".into()))?,
            )?;
            yearly_results.push(YearlyExecutionResult {
                result_id,
                item_id,
                base_daily_version: VersionRef {
                    dataset: Dataset::Daily,
                    contract_version: ContractVersion::V1,
                    entity: Some(e.clone()),
                    version_id: base_id.clone(),
                    integrity_sha256: base_id,
                },
                result_version: VersionRef {
                    dataset: Dataset::Yearly,
                    contract_version: ContractVersion::V1,
                    entity: Some(e),
                    version_id: new_id.clone(),
                    integrity_sha256: new_id,
                },
                no_change: outcome == 1,
                derived_at: when,
            });
        }
    }
    task_facts_check_evidenced(&s, &range_results, &yearly_results)?;
    Ok(TaskFacts {
        request_id: request,
        snapshot: s,
        publications,
        range_results,
        yearly_results,
    })
}

/// `range_result` on schema 21: the committed `market_events` row is the
/// result record; base/result refs are the old/new content digests.
fn range_result_series(c: &Connection, id: &str) -> Result<RangeExecutionResult, Error> {
    let e = crate::sqlite_market::series::event_by_result(c, id)?
        .ok_or_else(|| Error::Integrity("range result missing".into()))?;
    if e.period != 1 || e.kind == "first_publish" {
        return Err(Error::Integrity("range result kind/period mismatch".into()));
    }
    let entity = crate::data_supply::observe::entity(&e.entity)
        .ok_or_else(|| Error::Integrity("range result entity missing".into()))?;
    let base_id = crate::compact_market::digest_hex(
        &e.old_content_digest
            .ok_or_else(|| Error::Integrity("range result lacks base".into()))?,
    );
    let new_id = crate::compact_market::digest_hex(&e.new_content_digest);
    Ok(RangeExecutionResult {
        result_id: e.result_id,
        item_id: e.item_id,
        base_version: VersionRef {
            dataset: Dataset::Daily,
            contract_version: ContractVersion::V1,
            entity: Some(entity.clone()),
            version_id: base_id.clone(),
            integrity_sha256: base_id,
        },
        result_version: VersionRef {
            dataset: Dataset::Daily,
            contract_version: ContractVersion::V1,
            entity: Some(entity),
            version_id: new_id.clone(),
            integrity_sha256: new_id,
        },
        no_change: e.outcome == 1,
        acquired_at: series_timestamp(&e.created_at)?,
    })
}

/// `yearly_result` on schema 21: the `yearly_rederive` event carries the
/// daily base digest it derived from.
fn yearly_result_series(c: &Connection, id: &str) -> Result<YearlyExecutionResult, Error> {
    let e = crate::sqlite_market::series::event_by_result(c, id)?
        .ok_or_else(|| Error::Integrity("yearly result missing".into()))?;
    if e.period != 2 {
        return Err(Error::Integrity("yearly result period mismatch".into()));
    }
    let entity = crate::data_supply::observe::entity(&e.entity)
        .ok_or_else(|| Error::Integrity("yearly result entity".into()))?;
    let base_id = crate::compact_market::digest_hex(
        &e.base_content_digest
            .ok_or_else(|| Error::Integrity("yearly result lacks daily base".into()))?,
    );
    let new_id = crate::compact_market::digest_hex(&e.new_content_digest);
    Ok(YearlyExecutionResult {
        result_id: e.result_id,
        item_id: e.item_id,
        base_daily_version: VersionRef {
            dataset: Dataset::Daily,
            contract_version: ContractVersion::V1,
            entity: Some(entity.clone()),
            version_id: base_id.clone(),
            integrity_sha256: base_id,
        },
        result_version: VersionRef {
            dataset: Dataset::Yearly,
            contract_version: ContractVersion::V1,
            entity: Some(entity),
            version_id: new_id.clone(),
            integrity_sha256: new_id,
        },
        no_change: e.outcome == 1,
        derived_at: series_timestamp(&e.created_at)?,
    })
}

#[cfg(test)]
mod xp025_tests {
    use super::*;

    fn plan(c: &Connection, sql: &str) -> Vec<String> {
        let mut q = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        // The statements under test take bound parameters; plans do not
        // depend on their values, so bind NULLs.
        let nulls = vec![rusqlite::types::Null; q.parameter_count()];
        q.query_map(rusqlite::params_from_iter(nulls), |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }
    fn db() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let tasks = crate::sqlite_tasks::SqliteTasks::new(dir.path().join("t.sqlite"));
        tasks.initialize().unwrap();
        let c = Connection::open(tasks.path()).unwrap();
        (dir, c)
    }
    /// Statement shapes, not timing: the list scan reads only `tasks`
    /// (never items/requests/scopes) and the related lookup is one pass over
    /// the narrow `task_scopes` table joined to `tasks` by primary key.
    #[test]
    fn task_listing_and_related_lookup_plans_are_bounded() {
        let (_dir, c) = db();
        let scan = crate::data_supply::observe::TaskScan {
            dataset: Some("market.daily".into()),
            operation: Some("start_collection".into()),
            stored_statuses: Some(vec!["succeeded".into()]),
            created_from: Some(chrono::Utc::now()),
            created_to: Some(chrono::Utc::now()),
            upper: Some(chrono::Utc::now()),
            after: Some(("2026-01-01T00:00:00+00:00".into(), "task-x".into())),
            batch: 50,
        };
        let (sql, _) = scan_tasks_sql(&scan);
        assert!(sql.ends_with("LIMIT 50"), "{sql}");
        for detail in plan(&c, &sql) {
            if let Some(rest) = detail.strip_prefix("SCAN ") {
                assert!(
                    rest.starts_with("tasks"),
                    "list scan may only scan tasks: {detail}"
                );
            }
        }
        let related = related_scopes_sql(3);
        for detail in plan(&c, &related) {
            if let Some(rest) = detail.strip_prefix("SCAN ") {
                assert!(
                    rest.starts_with("s ") || rest == "s",
                    "related lookup may only scan task_scopes: {detail}"
                );
            }
        }
        assert!(
            plan(&c, &related)
                .iter()
                .any(|d| d.contains("SEARCH t USING") && d.contains("task_id=?")),
            "tasks must be reached by primary key"
        );
    }
}
