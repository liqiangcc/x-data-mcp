//! SQLite mapping for Stage B. Explicit shared task-schema initialization only.
use crate::{
    market_data::{Bar, Period},
    market_storage::{
        compat::{self, CheckedRecord},
        *,
    },
    sqlite_tasks::SqliteTasks,
};
use chrono::{Datelike, FixedOffset, NaiveDate};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use rust_decimal::Decimal;
use serde_json::Value;
#[derive(Clone)]
pub struct SqliteMarket {
    pub tasks: SqliteTasks,
}
impl SqliteMarket {
    pub fn new(tasks: SqliteTasks) -> Self {
        Self { tasks }
    }
    fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T, Error>) -> Result<T, Error> {
        let mut c = self.tasks.connect(true)?;
        let tx = c.transaction().map_err(storage)?;
        crate::sqlite_tasks::check_schema(&tx, true)?;
        let result = f(&tx)?;
        tx.commit().map_err(storage)?;
        Ok(result)
    }
    fn write<T>(&self, f: impl FnOnce(&Connection) -> Result<T, Error>) -> Result<T, Error> {
        self.write_transaction(false, f)
    }
    fn write_transaction<T>(
        &self,
        _publication: bool,
        f: impl FnOnce(&Connection) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let mut c = self.tasks.connect(false)?;
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        crate::sqlite_tasks::check_schema(&tx, true)?;
        let result = f(&tx)?;
        #[cfg(test)]
        if _publication {
            tests::crash_point("before_commit");
        }
        tx.commit()
            .map_err(|e| Error::OutcomeUnknown(e.to_string()))?;
        #[cfg(test)]
        if _publication {
            tests::crash_point("after_commit");
        }
        Ok(result)
    }
    pub fn stage(&self, raw: &Value) -> Result<(), Error> {
        let record = compat::check(raw)?;
        self.write(|c| {
            let base = base_record(c, &record)?;
            validate_base_identity(&record, base.as_ref())?;
            insert_record(c, &record, base.as_ref())
        })
    }
    pub fn candidate(&self, id: &str) -> Result<CheckedRecord, Error> {
        self.read(|c| read_record(c, id, false, true))
    }
    pub fn fixed(&self, id: &str) -> Result<CheckedRecord, Error> {
        self.read(|c| read_record(c, id, true, true))
    }
    pub fn range(&self, scope: &Scope, id: Option<&str>, range: &Range) -> Result<ReadPage, Error> {
        range.validate()?;
        self.read(|c| {
            let version_missing_from_head = id.is_none();
            let id = match id {
                Some(id) => id.to_owned(),
                None => head(c, scope)?.ok_or(Error::NotFound)?,
            };
            let record = read_record(c, &id, true, true).map_err(|e| {
                if version_missing_from_head && matches!(&e, Error::NotFound) {
                    integrity("effective head target unavailable")
                } else {
                    e
                }
            })?;
            if Scope::of(&record.version) != *scope {
                return Err(Error::Integrity("version/scope mismatch".into()));
            }
            // Full verification above is O(version rows). This SELECT is separately indexed/bounded.
            #[cfg(test)]
            tests::after_verification();
            let rows = range_rows(c, &id, scope.period, range)?;
            let more = rows.len() > range.limit;
            let rows = rows.into_iter().take(range.limit).collect::<Vec<_>>();
            let next_after = if more {
                rows.last().map(|b| b.date)
            } else {
                None
            };
            Ok(ReadPage {
                integrity_sha256: record.raw["integrity_sha256"].as_str().unwrap().to_owned(),
                version: record.version,
                rows,
                next_after,
            })
        })
    }
    pub fn versions(&self, scope: &Scope) -> Result<Vec<crate::publish::EffectiveVersion>, Error> {
        self.read(|c|{
            let mut stmt=c.prepare("SELECT version_id FROM data_versions WHERE symbol=?1 AND period=?2 AND adjustment='qfq' AND state='published' ORDER BY published_at DESC,version_id ASC").map_err(storage)?;
            let ids=stmt.query_map(params![scope.symbol,period(scope.period)],|r|r.get::<_,String>(0)).map_err(storage)?.collect::<Result<Vec<_>,_>>().map_err(storage)?;
            ids.into_iter().map(|id|read_record(c,&id,true,true).map(|r|r.version)).collect()
        })
    }
}
fn storage(e: impl std::fmt::Display) -> Error {
    Error::Storage(e.to_string())
}
fn integrity(e: impl std::fmt::Display) -> Error {
    Error::Integrity(e.to_string())
}
fn text<T: serde::Serialize>(v: &T) -> Result<String, Error> {
    serde_json::to_string(v).map_err(integrity)
}
fn period(p: Period) -> &'static str {
    match p {
        Period::Day => "day",
        Period::Year => "year",
    }
}
fn decimal(v: Decimal) -> String {
    v.normalize().to_string()
}
fn as_of(r: &CheckedRecord) -> NaiveDate {
    r.version
        .published_at
        .with_timezone(&FixedOffset::east_opt(8 * 3600).unwrap())
        .date_naive()
}
fn base_record(c: &Connection, r: &CheckedRecord) -> Result<Option<CheckedRecord>, Error> {
    r.version
        .yearly_derivation
        .as_ref()
        .map(|l| read_record(c, &l.base_daily_version_id, true, false).map_err(base_error))
        .transpose()
}
fn head(c: &Connection, scope: &Scope) -> Result<Option<String>, Error> {
    c.query_row(
        "SELECT version_id FROM effective_heads WHERE symbol=?1 AND period=?2 AND adjustment='qfq'",
        params![scope.symbol, period(scope.period)],
        |r| r.get(0),
    )
    .optional()
    .map_err(storage)
}
fn metadata(r: &CheckedRecord) -> Vec<String> {
    let v = &r.version;
    vec![
        v.symbol.clone(),
        period(v.period).into(),
        "qfq".into(),
        v.source.clone(),
        v.published_at.to_rfc3339(),
        v.validation.record_count.to_string(),
        v.validation.unique_record_count.to_string(),
        v.validation.duplicate_count.to_string(),
        format!("{:?}", r.batch.coverage.status),
        r.batch.coverage.reason.clone(),
        v.records[0].date.to_string(),
        v.records.last().unwrap().date.to_string(),
    ]
}
fn evidence(r: &CheckedRecord) -> Result<Vec<Vec<Option<String>>>, Error> {
    let u = &r.version.unit_currency;
    let mut rows = vec![];
    for (field, e) in [
        ("price_currency", &u.price_currency),
        ("price_unit", &u.price_unit),
        ("volume_unit", &u.volume_unit),
        ("amount_currency", &u.amount_currency),
        ("amount_unit", &u.amount_unit),
    ] {
        rows.push(vec![
            Some(field.into()),
            Some(text(&e.status)?),
            e.value.clone(),
            Some(e.reason.clone()),
        ]);
    }
    let e = &r.version.adjustment_evidence;
    rows.push(vec![
        Some("adjustment".into()),
        Some(text(&e.status)?),
        e.value.clone(),
        Some(e.reason.clone()),
    ]);
    rows.sort();
    Ok(rows)
}
fn bar_values(b: &Bar) -> Vec<Option<String>> {
    vec![
        Some(b.date.to_string()),
        Some(decimal(b.open)),
        Some(decimal(b.high)),
        Some(decimal(b.low)),
        Some(decimal(b.close)),
        b.volume.map(decimal),
        b.amount.map(decimal),
    ]
}
fn annual_values(
    r: &CheckedRecord,
    base: &CheckedRecord,
) -> Result<Vec<Vec<Option<String>>>, Error> {
    // Mapping of fixed dates/flags only; numeric aggregation belongs to application validation.
    r.version
        .records
        .iter()
        .map(|bar| {
            let dates = base
                .version
                .records
                .iter()
                .filter(|b| b.date.year() == bar.date.year())
                .map(|b| b.date)
                .collect::<Vec<_>>();
            let first = dates
                .first()
                .ok_or_else(|| Error::Base("year absent from fixed base".into()))?;
            let last = dates.last().unwrap();
            let mut row = vec![Some(bar.date.year().to_string())];
            row.extend(bar_values(bar));
            row.extend([
                Some(first.to_string()),
                Some(last.to_string()),
                Some(i32::from(bar.date.year() < as_of(r).year()).to_string()),
            ]);
            Ok(row)
        })
        .collect()
}
fn lineage_values(r: &CheckedRecord) -> Option<Vec<Option<String>>> {
    r.version.yearly_derivation.as_ref().map(|l| {
        vec![
            Some(l.base_daily_version_id.clone()),
            Some(l.algorithm_version.clone()),
            Some(l.first_trading_date.to_string()),
            Some(l.last_trading_date.to_string()),
            Some(as_of(r).to_string()),
        ]
    })
}
fn insert_record(
    c: &Connection,
    r: &CheckedRecord,
    base: Option<&CheckedRecord>,
) -> Result<(), Error> {
    let v = &r.version;
    let m = metadata(r);
    c.execute("INSERT INTO data_versions VALUES(?1,?2,?3,?4,'staging',?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",params![v.version_id,m[0],m[1],m[2],m[3],m[4],m[5],m[6],m[7],m[8],m[9],m[10],m[11],text(&r.raw)?,r.raw["integrity_sha256"].as_str(),compat::DIGEST_FORMAT]).map_err(storage)?;
    for e in evidence(r)? {
        c.execute(
            "INSERT INTO version_evidence VALUES(?1,?2,?3,?4,?5)",
            params![v.version_id, e[0], e[1], e[2], e[3]],
        )
        .map_err(storage)?;
    }
    if v.period == Period::Day {
        for b in &v.records {
            let b = bar_values(b);
            c.execute(
                "INSERT INTO daily_bars VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![v.version_id, b[0], b[1], b[2], b[3], b[4], b[5], b[6]],
            )
            .map_err(storage)?;
        }
    } else {
        for b in annual_values(r, base.ok_or_else(|| Error::Base("missing base".into()))?)? {
            c.execute(
                "INSERT INTO yearly_bars VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                params![
                    v.version_id,
                    b[0],
                    b[1],
                    b[2],
                    b[3],
                    b[4],
                    b[5],
                    b[6],
                    b[7],
                    b[8],
                    b[9],
                    b[10]
                ],
            )
            .map_err(storage)?;
        }
        let l = lineage_values(r).unwrap();
        c.execute(
            "INSERT INTO yearly_lineage VALUES(?1,?2,?3,?4,?5,?6)",
            params![v.version_id, l[0], l[1], l[2], l[3], l[4]],
        )
        .map_err(storage)?;
    }
    Ok(())
}
fn rows(c: &Connection, sql: &str, id: &str, n: usize) -> Result<Vec<Vec<Option<String>>>, Error> {
    let mut stmt = c.prepare(sql).map_err(storage)?;
    stmt.query_map([id], |r| {
        (0..n).map(|i| r.get::<_, Option<String>>(i)).collect()
    })
    .map_err(storage)?
    .collect::<Result<Vec<_>, _>>()
    .map_err(storage)
}
fn read_record(
    c: &Connection,
    id: &str,
    published: bool,
    allow_yearly: bool,
) -> Result<CheckedRecord, Error> {
    let record = read_projection(c, id, published, allow_yearly)?;
    let base = base_record(c, &record)?;
    validate_base(&record, base.as_ref())?;
    Ok(record)
}
fn read_projection(
    c: &Connection,
    id: &str,
    published: bool,
    allow_yearly: bool,
) -> Result<CheckedRecord, Error> {
    let row:Option<(String,String,String,String)>=c.query_row("SELECT compat_json,content_digest,digest_format,state FROM data_versions WHERE version_id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(storage)?;
    let publication_exists: bool = c
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM publication_batches WHERE version_id=?1)",
            [id],
            |r| r.get(0),
        )
        .map_err(storage)?;
    let (raw, digest, format, state) = row.ok_or_else(|| {
        if publication_exists {
            integrity("published version record missing")
        } else {
            Error::NotFound
        }
    })?;
    if published && state != "published" {
        return Err(if publication_exists {
            integrity("published version state changed")
        } else {
            Error::NotFound
        });
    }
    let raw: Value = serde_json::from_str(&raw).map_err(integrity)?;
    let record = compat::check(&raw)?;
    if record.version.version_id != id
        || digest != raw["integrity_sha256"].as_str().unwrap_or("")
        || format != compat::DIGEST_FORMAT
    {
        return Err(integrity("compat/version identity"));
    }
    if !allow_yearly && record.version.period != Period::Day {
        return Err(Error::Base("base period is not daily".into()));
    }
    if state == "published" {
        check_publication_link(c, id)?;
    }
    let base = base_record(c, &record)?;
    validate_base_identity(&record, base.as_ref())?;
    let actual = rows(
        c,
        "SELECT symbol,period,adjustment,source,published_at,CAST(record_count AS TEXT),CAST(unique_count AS TEXT),CAST(duplicate_count AS TEXT),coverage_status,coverage_reason,first_date,last_date FROM data_versions WHERE version_id=?1",
        id,
        12,
    )?;
    if actual != vec![metadata(&record).into_iter().map(Some).collect::<Vec<_>>()] {
        return Err(integrity("version metadata projection"));
    }
    if rows(
        c,
        "SELECT field,status,value,reason FROM version_evidence WHERE version_id=?1 ORDER BY field",
        id,
        4,
    )? != evidence(&record)?
    {
        return Err(integrity("unit/adjustment projection"));
    }
    let daily = rows(
        c,
        "SELECT trade_date,open,high,low,close,volume,amount FROM daily_bars WHERE version_id=?1 ORDER BY trade_date",
        id,
        7,
    )?;
    let yearly = rows(
        c,
        "SELECT CAST(year AS TEXT),trade_date,open,high,low,close,volume,amount,first_trade_date,last_trade_date,CAST(is_finished AS TEXT) FROM yearly_bars WHERE version_id=?1 ORDER BY year",
        id,
        11,
    )?;
    let lineage = rows(
        c,
        "SELECT base_daily_version_id,algorithm_version,first_trade_date,last_trade_date,as_of FROM yearly_lineage WHERE version_id=?1",
        id,
        5,
    )?;
    if record.version.period == Period::Day {
        if daily
            != record
                .version
                .records
                .iter()
                .map(bar_values)
                .collect::<Vec<_>>()
            || !yearly.is_empty()
            || !lineage.is_empty()
        {
            return Err(integrity("daily projection"));
        }
    } else if !daily.is_empty()
        || yearly != annual_values(&record, base.as_ref().unwrap())?
        || lineage != vec![lineage_values(&record).unwrap()]
    {
        return Err(integrity("yearly/lineage projection"));
    }
    Ok(record)
}
fn range_rows(c: &Connection, id: &str, p: Period, range: &Range) -> Result<Vec<Bar>, Error> {
    let (sql, start, end) = match p {
        Period::Day => (
            "SELECT trade_date,open,high,low,close,volume,amount FROM daily_bars WHERE version_id=?1 AND trade_date>=?2 AND trade_date<=?3 AND (?4 IS NULL OR trade_date>?4) ORDER BY trade_date LIMIT ?5",
            range.start.to_string(),
            range.end.to_string(),
        ),
        Period::Year => (
            "SELECT trade_date,open,high,low,close,volume,amount FROM yearly_bars WHERE version_id=?1 AND year>=CAST(?2 AS INTEGER) AND year<=CAST(?3 AS INTEGER) AND (?4 IS NULL OR trade_date>?4) ORDER BY year LIMIT ?5",
            range.start.year().to_string(),
            range.end.year().to_string(),
        ),
    };
    let mut stmt = c.prepare(sql).map_err(storage)?;
    let values = stmt
        .query_map(
            params![
                id,
                start,
                end,
                range.after.map(|d| d.to_string()),
                range.limit as u32 + 1
            ],
            |r| {
                (0..7)
                    .map(|i| r.get::<_, Option<String>>(i))
                    .collect::<Result<Vec<_>, _>>()
            },
        )
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    values
        .into_iter()
        .map(|v| {
            let value = |i: usize| {
                v[i].as_deref()
                    .ok_or_else(|| integrity("missing numeric/date field"))
            };
            Ok(Bar {
                date: value(0)?.parse().map_err(integrity)?,
                open: value(1)?.parse().map_err(integrity)?,
                high: value(2)?.parse().map_err(integrity)?,
                low: value(3)?.parse().map_err(integrity)?,
                close: value(4)?.parse().map_err(integrity)?,
                volume: v[5]
                    .as_ref()
                    .map(|s| s.parse())
                    .transpose()
                    .map_err(integrity)?,
                amount: v[6]
                    .as_ref()
                    .map(|s| s.parse())
                    .transpose()
                    .map_err(integrity)?,
            })
        })
        .collect()
}

fn check_publication_link(c: &Connection, version: &str) -> Result<(), Error> {
    let row:Option<(String,String,String,String)>=c.query_row(
        "SELECT b.publication_id,b.command_json,b.receipt_json,b.task_id FROM publication_batches b JOIN task_publications p ON p.publication_id=b.publication_id AND p.task_id=b.task_id AND p.version_id=b.version_id WHERE b.version_id=?1",[version],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))
    ).optional().map_err(storage)?;
    let (id, command, receipt, task) =
        row.ok_or_else(|| integrity("published version lacks atomic publication evidence"))?;
    let command: Publication = serde_json::from_str(&command).map_err(integrity)?;
    let receipt: Receipt = serde_json::from_str(&receipt).map_err(integrity)?;
    if command.publication_id != id
        || command.version_id != version
        || command.task_id != task
        || receipt.publication_id != id
        || receipt.version_id != version
        || receipt.task_id != task
    {
        return Err(integrity("publication association mismatch"));
    }
    Ok(())
}
impl MarketRepository for SqliteMarket {
    fn stage(&self, raw: &Value) -> Result<(), Error> {
        SqliteMarket::stage(self, raw)
    }
    fn candidate(&self, id: &str) -> Result<CheckedRecord, Error> {
        SqliteMarket::candidate(self, id)
    }
    fn fixed(&self, id: &str) -> Result<CheckedRecord, Error> {
        SqliteMarket::fixed(self, id)
    }
    fn range(&self, scope: &Scope, id: Option<&str>, range: &Range) -> Result<ReadPage, Error> {
        SqliteMarket::range(self, scope, id, range)
    }
    fn versions(&self, scope: &Scope) -> Result<Vec<crate::publish::EffectiveVersion>, Error> {
        SqliteMarket::versions(self, scope)
    }
    fn fixed_many(&self, ids: &[String]) -> Result<Vec<CheckedRecord>, Error> {
        self.read(|c| {
            ids.iter()
                .map(|id| read_record(c, id, true, true))
                .collect()
        })
    }
    fn task(&self, id: &str) -> Result<crate::task_control::TaskSnapshot, Error> {
        self.read(|c| {
            crate::sqlite_tasks::read_bound_task(c, id)?
                .ok_or(Error::Task(crate::task_control::Error::TaskNotFound))
        })
    }
    fn mark_validated(&self, record: &CheckedRecord) -> Result<(), Error> {
        self.write(|c|{
            let current=read_projection(c,&record.version.version_id,false,true)?;
            if current.raw!=record.raw{return Err(integrity("candidate changed during validation"));}
            let n=c.execute("UPDATE data_versions SET state='validated' WHERE version_id=?1 AND state IN ('staging','validated')",[&record.version.version_id]).map_err(storage)?;
            if n!=1{return Err(Error::Conflict("candidate already published".into()));}Ok(())
        })
    }
    fn receipt(&self, id: &str) -> Result<Option<Receipt>, Error> {
        self.read(|c| {
            let raw: Option<String> = c
                .query_row(
                    "SELECT receipt_json FROM publication_batches WHERE publication_id=?1",
                    [id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(storage)?;
            raw.map(|raw| {
                let receipt: Receipt = serde_json::from_str(&raw).map_err(integrity)?;
                read_record(c, &receipt.version_id, true, true)?;
                Ok(receipt)
            })
            .transpose()
        })
    }
    fn commit(&self, plan: &PublishPlan) -> Result<Receipt, Error> {
        self.write_transaction(true,|c|{
            let command=&plan.command;
            let old:Option<(String,String)>=c.query_row("SELECT command_json,receipt_json FROM publication_batches WHERE publication_id=?1",[&command.publication_id],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(storage)?;
            if let Some((raw,receipt))=old {
                if serde_json::from_str::<Publication>(&raw).map_err(integrity)?!=*command{return Err(Error::Conflict("publication identity reused with different command".into()));}
                let stored=read_projection(c,&command.version_id,true,true)?;
                if stored.raw!=plan.record.raw{return Err(integrity("published record differs from prepared command"));}
                return serde_json::from_str(&receipt).map_err(integrity);
            }
            let record=read_projection(c,&command.version_id,false,true)?;
            if record.raw!=plan.record.raw{return Err(integrity("validated candidate changed"));}
            let state:String=c.query_row("SELECT state FROM data_versions WHERE version_id=?1",[&command.version_id],|r|r.get(0)).map_err(storage)?;
            if state!="validated"{return Err(Error::Conflict("candidate is not validated".into()));}
            let base = base_record(c, &record)?;
            if base.as_ref().map(|r| r.raw["integrity_sha256"].as_str().unwrap().to_owned()) != plan.base_digest {
                return Err(Error::Base("fixed base changed since validation".into()));
            }
            // Only the application plan chooses required heads and whether to promote.
            for (scope,expected) in &plan.required_heads {if head(c,scope)?!=*expected{return Err(Error::Conflict("effective head changed".into()));}}
            let task=crate::sqlite_tasks::read_bound_task(c,&command.task_id)?.ok_or(Error::Task(crate::task_control::Error::TaskNotFound))?;
            let next=plan.finish_task(task)?;
            let receipt=Receipt{publication_id:command.publication_id.clone(),version_id:command.version_id.clone(),task_id:command.task_id.clone(),task_revision:next.task.state_revision};
            c.execute("INSERT INTO publication_batches VALUES(?1,?2,?3,?4,?5,?6)",params![command.publication_id,command.version_id,command.task_id,command.item_id,text(command)?,text(&receipt)?]).map_err(storage)?;
            c.execute("INSERT INTO task_publications VALUES(?1,?2,?3)",params![command.task_id,command.version_id,command.publication_id]).map_err(storage)?;
            c.execute("UPDATE data_versions SET state='published' WHERE version_id=?1 AND state='validated'",[&command.version_id]).map_err(storage)?;
            if let Some(scope)=&plan.promote {
                let changed = c.execute("INSERT INTO effective_heads VALUES(?1,?2,'qfq',?3) ON CONFLICT(symbol,period,adjustment) DO UPDATE SET version_id=excluded.version_id",params![scope.symbol,period(scope.period),command.version_id]).map_err(storage)?;
                if changed != 1 { return Err(Error::Conflict("head promotion did not write one row".into())); }
            }
            crate::sqlite_tasks::write_task(c,&next,command.expected_revision)?;
            read_projection(c,&command.version_id,true,true)?;
            Ok(receipt)
        })
    }
}

#[cfg(test)]
mod tests;
