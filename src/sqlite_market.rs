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
    pub(crate) fn read<T>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T, Error>,
    ) -> Result<T, Error> {
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
            // Lost version metadata cannot be assigned reliably to a scope: fail closed.
            let orphan: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM publication_batches b LEFT JOIN data_versions v ON v.version_id=b.version_id WHERE v.version_id IS NULL)", [], |r|r.get(0)).map_err(storage)?;
            if orphan { return Err(integrity("publication references missing version")); }
            let mut stmt=c.prepare("SELECT version_id FROM data_versions v WHERE symbol=?1 AND period=?2 AND adjustment='qfq' AND (state='published' OR EXISTS(SELECT 1 FROM publication_batches b WHERE b.version_id=v.version_id)) ORDER BY published_at DESC,version_id ASC").map_err(storage)?;
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
pub(crate) fn head(c: &Connection, scope: &Scope) -> Result<Option<String>, Error> {
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
    let (env, payloads) = envelope::pack(&r.raw)?;
    envelope::write_payloads(c, &payloads)?;
    let evidence_digest = crate::hash::sha256(text(&serde_json::json!(evidence(r)?))?.as_bytes());
    c.execute("INSERT INTO data_versions(version_id,symbol,period,adjustment,state,source,published_at,record_count,unique_count,duplicate_count,coverage_status,coverage_reason,first_date,last_date,envelope_json,content_digest,digest_format,evidence_digest) VALUES(?1,?2,?3,?4,'staging',?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",params![v.version_id,m[0],m[1],m[2],m[3],m[4],m[5],m[6],m[7],m[8],m[9],m[10],m[11],text(&env)?,r.raw["integrity_sha256"].as_str(),compat::DIGEST_FORMAT,evidence_digest]).map_err(storage)?;
    if v.period == Period::Day {
        write_day_segments(c, r)?;
    } else {
        let mut bar_stmt = c
            .prepare("INSERT INTO yearly_bars VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)")
            .map_err(storage)?;
        for b in annual_values(r, base.ok_or_else(|| Error::Base("missing base".into()))?)? {
            bar_stmt
                .execute(params![
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
                ])
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
enum SegmentPiece {
    /// Positions [first,last] of the head version's records, reused by
    /// manifest subtree reference.
    Base {
        first: usize,
        last: usize,
    },
    New {
        first: usize,
        last: usize,
    },
}
fn write_day_segments(c: &Connection, r: &CheckedRecord) -> Result<(), Error> {
    let v = &r.version;
    let scope = Scope::of(v);
    let head_id = head(c, &scope)?;
    let mut base: Vec<(String, String)> = Vec::new();
    let mut base_root: Option<i64> = None;
    if let Some(head_id) = head_id.as_deref().filter(|id| *id != v.version_id) {
        base_root = manifest::root_of(c, head_id)?;
        // A payload-backed head (a migrated sharer whose text differs from the
        // segments) has no manifest; it simply shares nothing this round.
        if let Some(root) = base_root {
            // Exact stored bar text of the head manifest: sharing requires
            // byte-identical content, not merely equal numbers.
            base = manifest::bars(c, root)?
                .iter()
                .map(|b| Ok((b["date"].as_str().unwrap_or("").to_owned(), text(b)?)))
                .collect::<Result<Vec<_>, Error>>()?;
        }
    }
    let mut base_index = std::collections::HashMap::new();
    for (i, row) in base.iter().enumerate() {
        base_index.insert(row.0.clone(), i);
    }
    let shareable = |b: &(String, String)| -> Option<usize> {
        let &idx = base_index.get(&b.0)?;
        (base[idx] == *b).then_some(idx)
    };
    let mut pieces: Vec<SegmentPiece> = Vec::new();
    let push_new = |first: usize, last: usize, pieces: &mut Vec<SegmentPiece>| {
        if let Some(SegmentPiece::New { last: l, .. }) = pieces.last_mut()
            && *l + 1 == first
        {
            *l = last;
            return;
        }
        pieces.push(SegmentPiece::New { first, last });
    };
    // Exact bar content, byte-identical to raw["records"] elements; sharing a
    // subtree therefore also shares the exact reconstructed representation.
    let bar_jsons = r.raw["records"]
        .as_array()
        .ok_or_else(|| integrity("record lacks records array"))?
        .iter()
        .map(|b| Ok((b["date"].as_str().unwrap_or("").to_owned(), text(b)?)))
        .collect::<Result<Vec<(String, String)>, Error>>()?;
    if bar_jsons.len() != v.records.len() {
        return Err(integrity("record/records array mismatch"));
    }
    let mut i = 0;
    while i < bar_jsons.len() {
        let b = &bar_jsons[i];
        match shareable(b) {
            Some(idx) => {
                let first = idx;
                let mut last = idx;
                let mut j = i + 1;
                while j < bar_jsons.len() {
                    match shareable(&bar_jsons[j]) {
                        Some(nidx) if nidx == last + 1 => {
                            last = nidx;
                            j += 1;
                        }
                        _ => break,
                    }
                }
                pieces.push(SegmentPiece::Base { first, last });
                i = j;
            }
            None => {
                push_new(i, i, &mut pieces);
                i += 1;
            }
        }
    }
    let mut next_segment: i64 = c
        .query_row(
            "SELECT COALESCE(MAX(segment_id),0)+1 FROM bar_segments",
            [],
            |r| r.get(0),
        )
        .map_err(storage)?;
    let mut seg_stmt = c
        .prepare("INSERT INTO bar_segments VALUES(?1,?2,?3,?4,?5)")
        .map_err(storage)?;
    let mut bar_stmt = c
        .prepare("INSERT INTO segment_bars VALUES(?1,?2,?3)")
        .map_err(storage)?;
    let mut ops: Vec<manifest::Piece> = Vec::with_capacity(pieces.len());
    for piece in &pieces {
        match piece {
            SegmentPiece::Base { first, last } => ops.push(manifest::Piece::Base {
                first: *first,
                last: *last,
            }),
            SegmentPiece::New { first, last } => {
                let rows = &bar_jsons[*first..=*last];
                let segment_id = next_segment;
                next_segment += 1;
                seg_stmt
                    .execute(params![
                        segment_id,
                        v.symbol,
                        rows.first().unwrap().0,
                        rows.last().unwrap().0,
                        rows.len() as u32
                    ])
                    .map_err(storage)?;
                for (date, json) in rows {
                    bar_stmt
                        .execute(params![segment_id, date, json])
                        .map_err(storage)?;
                }
                ops.push(manifest::Piece::New {
                    segment: segment_id,
                    first_date: rows.first().unwrap().0.clone(),
                    last_date: rows.last().unwrap().0.clone(),
                    count: rows.len(),
                });
            }
        }
    }
    let root = manifest::build(c, base_root, &ops)?;
    c.execute(
        "UPDATE data_versions SET manifest_root=?1 WHERE version_id=?2",
        params![root, v.version_id],
    )
    .map_err(storage)?;
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
pub(crate) fn read_record(
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
    let row:Option<(String,String,String,String,String)>=c.query_row("SELECT envelope_json,content_digest,digest_format,state,evidence_digest FROM data_versions WHERE version_id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional().map_err(storage)?;
    let publication_exists: bool = c
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM publication_batches WHERE version_id=?1)",
            [id],
            |r| r.get(0),
        )
        .map_err(storage)?;
    let (raw, digest, format, state, evidence_digest) = row.ok_or_else(|| {
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
    let env: Value = serde_json::from_str(&raw).map_err(integrity)?;
    let raw = envelope::expand(c, Some(id), &env)?;
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
    if evidence_digest
        != crate::hash::sha256(text(&serde_json::json!(evidence(&record)?))?.as_bytes())
    {
        return Err(integrity("unit/adjustment projection"));
    }
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
        // A segment-backed record's expansion is the manifest itself; the
        // legacy digest already binds it. Manifest clip lengths are verified
        // during expansion, and payload-backed records are digest-proved.
        if !yearly.is_empty() || !lineage.is_empty() {
            return Err(integrity("daily projection"));
        }
    } else if yearly != annual_values(&record, base.as_ref().unwrap())?
        || lineage != vec![lineage_values(&record).unwrap()]
    {
        return Err(integrity("yearly/lineage projection"));
    }
    Ok(record)
}
pub(crate) fn range_rows(
    c: &Connection,
    id: &str,
    p: Period,
    range: &Range,
) -> Result<Vec<Bar>, Error> {
    if p == Period::Day {
        let segment_backed: bool = c
            .query_row(
                "SELECT json_extract(envelope_json,'$.records.$ref')='segments' FROM data_versions WHERE version_id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()
            .map_err(storage)?
            .ok_or(Error::NotFound)?;
        let jsons: Vec<String> = if segment_backed {
            let root = manifest::root_of(c, id)?
                .ok_or_else(|| integrity("segment-backed version lacks manifest"))?;
            let clips = manifest::clips_in_range(
                c,
                root,
                &range.start.to_string(),
                &range.end.to_string(),
            )?;
            let mut out = Vec::new();
            let mut remaining = range.limit as u32 + 1;
            let mut stmt = c
                .prepare(
                    "SELECT bar_json FROM segment_bars WHERE segment_id=?1 AND trade_date>=?2 AND trade_date<=?3 AND (?4 IS NULL OR trade_date>?4) ORDER BY trade_date LIMIT ?5",
                )
                .map_err(storage)?;
            for (seg, lo, hi) in clips {
                if remaining == 0 {
                    break;
                }
                let rows = stmt
                    .query_map(
                        params![seg, lo, hi, range.after.map(|d| d.to_string()), remaining],
                        |r| r.get::<_, String>(0),
                    )
                    .map_err(storage)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(storage)?;
                remaining -= rows.len() as u32;
                out.extend(rows);
            }
            out
        } else {
            // Payload-backed (migrated) records: their manifest bars may carry
            // different text, so the range filters the expanded record instead.
            let env: String = c
                .query_row(
                    "SELECT envelope_json FROM data_versions WHERE version_id=?1",
                    [id],
                    |r| r.get(0),
                )
                .map_err(storage)?;
            let env: Value = serde_json::from_str(&env).map_err(integrity)?;
            let raw = envelope::expand(c, Some(id), &env)?;
            let (start, end) = (range.start.to_string(), range.end.to_string());
            let after = range.after.map(|d| d.to_string());
            raw["records"]
                .as_array()
                .ok_or_else(|| integrity("record lacks records array"))?
                .iter()
                .filter(|b| {
                    let d = b["date"].as_str().unwrap_or("");
                    d >= start.as_str()
                        && d <= end.as_str()
                        && after.as_deref().map(|a| d > a).unwrap_or(true)
                })
                .take(range.limit + 1)
                .map(text)
                .collect::<Result<Vec<_>, _>>()?
        };
        return jsons
            .iter()
            .map(|j| {
                let mut bar: Bar = serde_json::from_str(j).map_err(integrity)?;
                // r7_raw is internal evidence, not part of the range wire value.
                bar.r7_raw = None;
                Ok(bar)
            })
            .collect();
    }
    let (sql, start, end) = match p {
        Period::Day => unreachable!(),
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
                r7_raw: None,
            })
        })
        .collect()
}

fn check_publication_link(c: &Connection, version: &str) -> Result<(), Error> {
    let row:Option<(String,String,String,String,String)>=c.query_row(
        "SELECT b.publication_id,b.command_json,b.receipt_json,b.task_id,b.item_id FROM publication_batches b JOIN task_publications p ON p.publication_id=b.publication_id AND p.task_id=b.task_id AND p.version_id=b.version_id WHERE b.version_id=?1",[version],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))
    ).optional().map_err(storage)?;
    let (id, command, receipt, task, item) =
        row.ok_or_else(|| integrity("published version lacks atomic publication evidence"))?;
    let command: Publication = serde_json::from_str(&command).map_err(integrity)?;
    let receipt: Receipt = serde_json::from_str(&receipt).map_err(integrity)?;
    if command.publication_id != id
        || command.version_id != version
        || command.task_id != task
        || command.item_id != item
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
    fn task_head(&self, id: &str) -> Result<crate::task_control::TaskHead, Error> {
        self.read(|c| {
            crate::sqlite_tasks::read_task_head(c, id)?
                .ok_or(Error::Task(crate::task_control::Error::TaskNotFound))
        })
    }
    fn task_item_binding(
        &self,
        task: &str,
        item: &str,
    ) -> Result<Option<crate::task_control::ItemBinding>, Error> {
        self.read(|c| crate::sqlite_tasks::read_item_binding(c, task, item).map_err(Error::Task))
    }
    fn transition_task_head(
        &self,
        head: &mut crate::task_control::TaskHead,
        item: Option<&mut crate::task_control::TaskItem>,
        run: &str,
        change: crate::task_control::Change,
    ) -> Result<(), Error> {
        self.write(|c| {
            crate::sqlite_tasks::transition_head_tx(c, head, item, run, change, chrono::Utc::now())
                .map_err(Error::Task)
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
                if receipt.publication_id != id {
                    return Err(integrity("receipt publication identity mismatch"));
                }
                read_record(c, &receipt.version_id, true, true)?;
                Ok(receipt)
            })
            .transpose()
        })
    }
    fn commit(&self, plan: &PublishPlan) -> Result<Receipt, Error> {
        self.write_transaction(true, |c| commit_publication(c, plan))
    }
}

fn commit_publication(c: &Connection, plan: &PublishPlan) -> Result<Receipt, Error> {
    let command = &plan.command;
    let old: Option<(String, String)> = c
        .query_row(
            "SELECT command_json,receipt_json FROM publication_batches WHERE publication_id=?1",
            [&command.publication_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(storage)?;
    if let Some((raw, receipt)) = old {
        if serde_json::from_str::<Publication>(&raw).map_err(integrity)? != *command {
            return Err(Error::Conflict(
                "publication identity reused with different command".into(),
            ));
        }
        let stored = read_projection(c, &command.version_id, true, true)?;
        if stored.raw != plan.record.raw {
            return Err(integrity("published record differs from prepared command"));
        }
        return serde_json::from_str(&receipt).map_err(integrity);
    }
    let reused: bool = c
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM market_update_results WHERE result_id=?1 UNION ALL SELECT 1 FROM yearly_execution_results WHERE result_id=?1)",
            [&command.publication_id],
            |r| r.get(0),
        )
        .map_err(storage)?;
    if reused {
        return Err(Error::Conflict(
            "publication id used by a range result".into(),
        ));
    }
    let record = read_projection(c, &command.version_id, false, true)?;
    if record.raw != plan.record.raw {
        return Err(integrity("validated candidate changed"));
    }
    let state: String = c
        .query_row(
            "SELECT state FROM data_versions WHERE version_id=?1",
            [&command.version_id],
            |r| r.get(0),
        )
        .map_err(storage)?;
    if state != "validated" {
        return Err(Error::Conflict("candidate is not validated".into()));
    }
    let base = base_record(c, &record)?;
    if base
        .as_ref()
        .map(|r| r.raw["integrity_sha256"].as_str().unwrap().to_owned())
        != plan.base_digest
    {
        return Err(Error::Base("fixed base changed since validation".into()));
    }
    // Only the application plan chooses required heads and whether to promote.
    for (scope, expected) in &plan.required_heads {
        if head(c, scope)? != *expected {
            return Err(Error::Conflict("effective head changed".into()));
        }
    }
    let mut binding =
        crate::sqlite_tasks::read_item_binding(c, &command.task_id, &command.item_id)?
            .ok_or(Error::Task(crate::task_control::Error::TaskNotFound))?;
    plan.finish_task(&mut binding)?;
    let receipt = Receipt {
        publication_id: command.publication_id.clone(),
        version_id: command.version_id.clone(),
        task_id: command.task_id.clone(),
        task_revision: binding.head.state_revision,
    };
    c.execute(
        "INSERT INTO publication_batches VALUES(?1,?2,?3,?4,?5,?6)",
        params![
            command.publication_id,
            command.version_id,
            command.task_id,
            command.item_id,
            text(command)?,
            text(&receipt)?
        ],
    )
    .map_err(storage)?;
    c.execute(
        "INSERT INTO task_publications VALUES(?1,?2,?3)",
        params![command.task_id, command.version_id, command.publication_id],
    )
    .map_err(storage)?;
    c.execute(
        "UPDATE data_versions SET state='published' WHERE version_id=?1 AND state='validated'",
        [&command.version_id],
    )
    .map_err(storage)?;
    if let Some(scope) = &plan.promote {
        let changed = c.execute("INSERT INTO effective_heads VALUES(?1,?2,'qfq',?3) ON CONFLICT(symbol,period,adjustment) DO UPDATE SET version_id=excluded.version_id",params![scope.symbol,period(scope.period),command.version_id]).map_err(storage)?;
        if changed != 1 {
            return Err(Error::Conflict(
                "head promotion did not write one row".into(),
            ));
        }
    }
    crate::sqlite_tasks::persist_decided(
        c,
        &binding.head,
        Some(&binding.item),
        command.expected_revision,
    )?;
    read_projection(c, &command.version_id, true, true)?;
    Ok(receipt)
}

pub(crate) mod collection;
pub(crate) mod envelope;
pub(crate) mod gaps;
pub(crate) mod manifest;
pub(crate) mod update;
pub(crate) mod yearly_results;

#[cfg(test)]
pub(crate) mod tests;
mod verification;

pub(crate) mod publication_checks;
