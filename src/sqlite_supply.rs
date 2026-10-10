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
                let compact=crate::sqlite_market::is_compact_schema(c)?;
                let orphan:bool=if compact{c.query_row("SELECT EXISTS(SELECT 1 FROM task_publications tp LEFT JOIN data_versions v ON v.version_key=tp.version_key WHERE v.version_key IS NULL)",[],|r|r.get(0))}else{c.query_row("SELECT EXISTS(SELECT 1 FROM publication_batches b LEFT JOIN data_versions v ON b.version_id=v.version_id WHERE v.version_id IS NULL)",[],|r|r.get(0))}.map_err(storage)?;if orphan{return Err(Error::Integrity("orphan market publication".into()));}
                let list_sql=if compact{"SELECT v.version_id FROM data_versions v WHERE v.entity=?1 AND v.period=?2 AND (v.state=3 OR EXISTS(SELECT 1 FROM task_publications tp WHERE tp.version_key=v.version_key)) ORDER BY v.published_at DESC,v.version_id ASC"}else{"SELECT version_id FROM data_versions v WHERE symbol=?1 AND period=?2 AND (state='published' OR EXISTS(SELECT 1 FROM publication_batches b WHERE b.version_id=v.version_id)) ORDER BY published_at DESC,version_id ASC"};
                for entity in entities {
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
    // Published compact heads answer the same facts without a full record
    // materialization: the slim projection's manifest/metadata are
    // byte-identical, and the in-window stats come from the windowed leaf
    // read (chain linkage + clip digests still enforced) instead of the
    // materialized `records` list.
    if let Some(r) = slim_market(c, ctx, plan.args.dataset, entity, &id)? {
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

fn metadata(r: &crate::market_storage::compat::CheckedRecord) -> Metadata {
    let v = r.version();
    let u = &v.unit_currency;
    let units = [
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
    .collect();
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
        units,
        adjustment: Some(Evidence {
            field: "price_adjustment".into(),
            status: format!("{:?}", v.adjustment_evidence.status).to_lowercase(),
            value: v.adjustment_evidence.value.clone(),
            reason: v.adjustment_evidence.reason.clone(),
        }),
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
pub(crate) fn task_list(c: &Connection, access: &Access) -> Result<Vec<TaskFacts>, Error> {
    let mut q=c.prepare("SELECT task_id,command_key_json,frozen_work_json FROM tasks ORDER BY created_at DESC,task_id ASC").map_err(storage)?;
    let rows = q
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    let mut result = vec![];
    for (id, key, work) in rows {
        let key: crate::task_control::CommandKey = serde_json::from_str(&key).map_err(storage)?;
        let allowed = match key.dataset.as_str() {
            "task.verification" => *access == Access::local(),
            "market.daily" => access.allows(Dataset::Daily, None),
            "market.yearly" => access.allows(Dataset::Yearly, None),
            "securities.catalog" => access.allows(Dataset::Catalog, None),
            "legacy_market_compound" => {
                access.allows(Dataset::Daily, None) || access.allows(Dataset::Yearly, None)
            }
            _ => false,
        };
        if !allowed {
            continue;
        }
        let work: crate::task_control::FrozenWork = serde_json::from_str(&work).map_err(storage)?;
        if work.items.iter().any(|i| {
            i.target["symbol"].as_str().is_some_and(|s| {
                crate::data_supply::observe::entity(s)
                    .is_none_or(|e| access.securities.as_ref().is_some_and(|a| !a.contains(&e)))
            })
        }) {
            continue;
        }
        match task_facts_light(c, &id, access) {
            Ok(f) => result.push(f),
            Err(Error::Invalid(s)) if s == "forbidden" => continue,
            Err(e) => return Err(e),
        }
        if result.len() > 10000 {
            return Err(invalid("task snapshot budget exceeded"));
        }
    }
    Ok(result)
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
    fn task(&self, id: &str, access: &Access) -> Result<TaskFacts, Error> {
        self.market.read(|c| {
            let mut ctx = VerifyCtx::default();
            task_facts(c, &mut ctx, id, access, true)
        })
    }
    fn verify_versions(&self, refs: &[VersionRef]) -> Result<(), Error> {
        ReadRepository::verify(self, refs)
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
        self.market.read(|c| {
            let mut ctx = VerifyCtx::default();
            let tasks: Vec<std::rc::Rc<TaskFacts>> = task_list(c, access)?
                .into_iter()
                .map(std::rc::Rc::new)
                .collect();
            // One pass over the task set instead of an entities x tasks x
            // items scan with a TaskSnapshot clone per hit: each task index
            // is registered under every distinct item symbol (catalog tasks
            // under the entity-less selection). Order follows `task_list`.
            let mut catalog_tasks = vec![];
            let mut by_symbol: std::collections::HashMap<String, Vec<usize>> = Default::default();
            for (i, f) in tasks.iter().enumerate() {
                if f.snapshot.task.key.dataset == "securities.catalog" {
                    catalog_tasks.push(i);
                }
                let symbols: std::collections::BTreeSet<&str> = f
                    .snapshot
                    .items
                    .iter()
                    .filter_map(|i| i.spec.target["symbol"].as_str())
                    .collect();
                for symbol in symbols {
                    by_symbol.entry(symbol.into()).or_default().push(i);
                }
            }
            let mut out = vec![];
            let entities: Vec<Option<Security>> = if plan.args.dataset == Dataset::Catalog {
                vec![None]
            } else {
                plan.entities.iter().cloned().map(Some).collect()
            };
            for entity in entities {
                let related_tasks = match &entity {
                    Some(e) => by_symbol
                        .get(e.symbol().as_str())
                        .into_iter()
                        .flatten()
                        .copied()
                        .collect::<Vec<usize>>(),
                    None => catalog_tasks.clone(),
                }
                .into_iter()
                .map(|i| tasks[i].clone())
                .collect();
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
