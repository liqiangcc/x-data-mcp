//! Stage C reads: resolve every selected object under one real SQLite transaction.
use crate::{
    data_supply::{Access, query::*, wire::*},
    market_storage::{Error, Scope},
    sqlite_catalog,
    sqlite_market::{backend::MarketBackend, read_record},
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
fn catalog_ref(v: &crate::catalog::CatalogVersion) -> Result<VersionRef, Error> {
    Ok(VersionRef {
        dataset: Dataset::Catalog,
        contract_version: ContractVersion::V1,
        entity: None,
        version_id: v.version_id.clone(),
        integrity_sha256: sqlite_catalog::digest(v)?,
    })
}
fn read_market(
    c: &Connection,
    dataset: Dataset,
    entity: &Security,
    id: &str,
) -> Result<crate::market_storage::compat::CheckedRecord, Error> {
    let r = read_record(c, id, true, true)?;
    if r.version().symbol != entity.symbol() || Some(r.version().period) != dataset.period() {
        return Err(Error::Integrity("version entity/dataset mismatch".into()));
    }
    Ok(r)
}
impl ReadRepository for SqliteSupply {
    fn query(&self, plan: &QueryPlan, access: &Access) -> Result<QuerySnapshot, Error> {
        self.market.read(|c| query(c, plan, access))
    }
    fn verify(&self, refs: &[VersionRef]) -> Result<(), Error> {
        self.market.read(|c| {
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
                        &read_market(c, r.dataset, entity, &r.version_id)?,
                    )
                };
                if actual != r {
                    return Err(Error::Integrity("fixed version changed".into()));
                }
            }
            Ok(())
        })
    }
    fn versions(&self, args: &VersionsQuery, access: &Access) -> Result<Vec<VersionInfo>, Error> {
        let entities = entities(args.dataset, &args.selection, access)?;
        self.market.read(|c|{
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
                    for id in ids {let r=read_market(c,args.dataset,&entity,&id)?;result.push(VersionInfo{version_ref:market_ref(args.dataset,&entity,&r),published_at:r.version().published_at});}
                }
            }
            result.sort_by(|a,b|b.published_at.cmp(&a.published_at).then(a.version_ref.version_id.cmp(&b.version_ref.version_id)));Ok(result)
        })
    }
}
pub(crate) fn query(
    c: &Connection,
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
            let r = read_market(c, plan.args.dataset, entity, &id)?;
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
                        yearly_row_meta(c, &r, &b)?
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
    bar: &crate::market_data::Bar,
) -> Result<YearlyRowMeta, Error> {
    if !crate::sqlite_market::is_compact_schema(c)? {
        let (first, last, finished): (String, String, bool) = c
            .query_row(
                "SELECT first_trade_date,last_trade_date,is_finished FROM yearly_bars \
                 WHERE version_id=?1 AND trade_date=?2",
                params![r.version().version_id, bar.date.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(storage)?;
        return Ok((
            Some(first.parse().map_err(storage)?),
            Some(last.parse().map_err(storage)?),
            Some(!finished),
        ));
    }
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
    let base = read_record(c, &lineage.base_daily_version_id, true, false)?;
    let annual = crate::yearly::aggregate(&base.version, None, as_of)
        .map_err(|e| Error::Integrity(format!("yearly re-aggregation: {e}")))?;
    let record = annual
        .records
        .iter()
        .find(|rec| rec.bar.date == bar.date)
        .ok_or_else(|| Error::Integrity("yearly record missing from re-aggregation".into()))?;
    Ok((
        Some(record.lineage.first_trading_date),
        Some(record.lineage.last_trading_date),
        Some(record.year_unfinished),
    ))
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
fn task_facts(
    c: &Connection,
    id: &str,
    access: &Access,
    verify_outputs: bool,
) -> Result<TaskFacts, Error> {
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
        let r = read_record(c, &id, true, true)?;
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
        .map(|id| range_result(c, id))
        .collect::<Result<Vec<_>, _>>()?;
    let mut q = c
        .prepare(
            "SELECT result_id FROM yearly_execution_results WHERE task_id=?1 ORDER BY result_id",
        )
        .map_err(storage)?;
    let yearly_ids = q
        .query_map([id], |r| r.get::<_, String>(0))
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    let yearly_results = yearly_ids
        .iter()
        .map(|id| yearly_result(c, id))
        .collect::<Result<Vec<_>, _>>()?;
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
    Ok(TaskFacts {
        yearly_results,
        range_results,
        request_id: request,
        snapshot: s,
        publications,
    })
}
fn task_list(c: &Connection, access: &Access) -> Result<Vec<TaskFacts>, Error> {
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
        match task_facts(c, &id, access, false) {
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
impl ObservationRepository for SqliteSupply {
    fn tasks(&self, access: &Access) -> Result<Vec<TaskFacts>, Error> {
        self.market.read(|c| task_list(c, access))
    }
    fn task(&self, id: &str, access: &Access) -> Result<TaskFacts, Error> {
        self.market.read(|c| task_facts(c, id, access, true))
    }
    fn verify_versions(&self, refs: &[VersionRef]) -> Result<(), Error> {
        ReadRepository::verify(self, refs)
    }
    fn verify_publication_check(
        &self,
        receipt: &Option<crate::publication_verification::decision::Receipt>,
    ) -> Result<(), Error> {
        use crate::publication_verification::decision::AuditStore;
        if let Some(expected) = receipt
            && self
                .market
                .check_receipt(&expected.verification_task_id)?
                .as_ref()
                != Some(expected)
        {
            return Err(Error::Integrity(
                "publication check changed since snapshot".into(),
            ));
        }
        Ok(())
    }
    fn verify_stopped_check(
        &self,
        receipt: &Option<crate::task_verification::decision::Receipt>,
    ) -> Result<(), Error> {
        if let Some(expected) = receipt {
            self.market.read(|c| {
                let actual =
                    crate::sqlite_verification::read_receipt(c, &expected.verification_task_id)?
                        .map(|(_, r)| r);
                if actual.as_ref() != Some(expected) {
                    return Err(Error::Integrity(
                        "stopped check changed since snapshot".into(),
                    ));
                }
                Ok(())
            })?;
        }
        Ok(())
    }
    fn verify_yearly_results(&self, results: &[YearlyExecutionResult]) -> Result<(), Error> {
        if results.is_empty() {
            return Ok(());
        }
        self.market.read(|c| {
            for expected in results {
                if yearly_result(c, &expected.result_id)? != *expected {
                    return Err(Error::Integrity(
                        "yearly result changed since snapshot".into(),
                    ));
                }
            }
            Ok(())
        })
    }
    fn verify_range_results(&self, results: &[RangeExecutionResult]) -> Result<(), Error> {
        if results.is_empty() {
            return Ok(());
        }
        self.market.read(|c| {
            for expected in results {
                if range_result(c, &expected.result_id)? != *expected {
                    return Err(Error::Integrity(
                        "range result changed since snapshot".into(),
                    ));
                }
            }
            Ok(())
        })
    }
    fn availability(
        &self,
        plan: &QueryPlan,
        access: &Access,
    ) -> Result<Vec<AvailabilityFacts>, Error> {
        self.market.read(|c| {
            let tasks = task_list(c, access)?;
            let mut out = vec![];
            let entities: Vec<Option<Security>> = if plan.args.dataset == Dataset::Catalog {
                vec![None]
            } else {
                plan.entities.iter().cloned().map(Some).collect()
            };
            for entity in entities {
                let mut single = plan.clone();
                if let Some(e) = &entity {
                    single.entities = vec![e.clone()];
                }
                let related_tasks = tasks
                    .iter()
                    .filter(|f| {
                        f.snapshot.items.iter().any(|i| match &entity {
                            Some(e) => i.spec.target["symbol"] == e.symbol(),
                            None => f.snapshot.task.key.dataset == "securities.catalog",
                        })
                    })
                    .cloned()
                    .collect();
                let proven_gaps = match (&entity, plan.args.dataset.period()) {
                    (Some(e), Some(period)) => {
                        crate::sqlite_market::gaps::read_open_gaps(c, &e.symbol(), period)?
                    }
                    _ => vec![],
                };
                match query(c, &single, access) {
                    Ok(s) => {
                        let dates = s
                            .rows
                            .iter()
                            .filter_map(|r| match r {
                                Row::Market { date, .. } => Some(*date),
                                _ => None,
                            })
                            .collect::<Vec<_>>();
                        out.push(AvailabilityFacts {
                            entity,
                            manifest: s.manifest.into_iter().next(),
                            first_record: dates.first().copied(),
                            last_record: dates.last().copied(),
                            record_count: s.rows.len(),
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

fn range_result(c: &Connection, id: &str) -> Result<RangeExecutionResult, Error> {
    let (audit, receipt) = crate::sqlite_market::update::read_result(c, id)?
        .ok_or_else(|| Error::Integrity("range result missing".into()))?;
    let base_id = audit["evidence"]["base_version_id"]
        .as_str()
        .ok_or_else(|| Error::Integrity("range base reference missing".into()))?;
    // These reads share the task/receipt transaction; never query a latest head.
    let base = read_record(c, base_id, true, true)?;
    let result = read_record(c, &receipt.version_id, true, true)?;
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

fn yearly_result(c: &Connection, id: &str) -> Result<YearlyExecutionResult, Error> {
    let (audit, receipt) = crate::sqlite_market::yearly_results::read_result(c, id)?
        .ok_or_else(|| Error::Integrity("yearly result missing".into()))?;
    let base_id = audit["base_daily_version_id"]
        .as_str()
        .ok_or_else(|| Error::Integrity("yearly base missing".into()))?;
    let base = read_record(c, base_id, true, true)?;
    let result = read_record(c, &receipt.version_id, true, true)?;
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
