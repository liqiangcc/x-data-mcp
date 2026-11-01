//! `source_refresh_runs` storage (refresh R2, issue-446 §5.6).
//!
//! The refresh record is deliberately in SQLite rather than only in a task
//! result: the task retention period is undecided, while `verified_through`
//! must survive both process restarts and task expiry. The run row says what a
//! run did; `source_refresh_run_sources` says whether the run completed the
//! whole planned request set of one source (an observed empty window is an
//! unobserved gap, not a failure; only a complete source advances
//! `verified_through`); `source_refresh_run_members` names the snapshot selected for each
//! request, which is what R3's replay selection consumes.
//!
//! Layer position: this module persists neutral records only. Source names,
//! windows and plans stay in `crate::source_refresh`.

use rusqlite::{Connection, OptionalExtension, params};

use super::{integrity, storage};
use crate::market_storage::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunStart {
    pub run_id: String,
    pub mode: String,
    pub scope: String,
    pub started_at: String,
    pub task_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunFinish {
    pub status: String,
    pub finished_at: String,
    pub requests: u64,
    pub reused: u64,
    pub changed: u64,
    pub failed_windows: u64,
    pub unobserved_windows: u64,
    pub verified_through: Option<String>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRow {
    pub run_id: String,
    pub mode: String,
    pub status: String,
    pub scope: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub requests: u64,
    pub reused: u64,
    pub changed: u64,
    pub failed_windows: u64,
    pub unobserved_windows: u64,
    pub verified_through: Option<String>,
    pub task_id: Option<String>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSourceRow {
    pub run_id: String,
    pub source_id: String,
    pub class: String,
    pub complete: bool,
    pub requests: u64,
    pub snapshots: u64,
    pub changed: u64,
    pub unobserved_windows: u64,
    pub failed_windows: u64,
    pub verified_through: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunMemberRow {
    pub run_id: String,
    pub source_id: String,
    pub request_fingerprint: String,
    pub snapshot_id: String,
    pub page_count: Option<u32>,
    pub row_count: usize,
    pub as_of: String,
    pub fetched_at: String,
    pub changed: bool,
    pub window_start: Option<String>,
    pub window_end: Option<String>,
}

pub(crate) fn begin(c: &Connection, start: &RunStart) -> Result<(), Error> {
    c.execute(
        "INSERT INTO source_refresh_runs(\
             run_id,mode,status,scope,started_at,task_id) \
         VALUES(?1,?2,'running',?3,?4,?5)",
        params![
            start.run_id,
            start.mode,
            start.scope,
            start.started_at,
            start.task_id,
        ],
    )
    .map_err(storage)?;
    Ok(())
}

/// Record the run's members and per-source summary, then close the run, all in
/// one transaction: a half-written member set cannot claim a finished run.
pub(crate) fn finish(
    c: &Connection,
    run_id: &str,
    finish: &RunFinish,
    sources: &[RunSourceRow],
    members: &[RunMemberRow],
) -> Result<usize, Error> {
    let changed = c
        .execute(
            "UPDATE source_refresh_runs SET \
                 status=?2,finished_at=?3,requests=?4,reused=?5,changed=?6,\
                 failed_windows=?7,unobserved_windows=?8,verified_through=?9,detail=?10 \
             WHERE run_id=?1 AND status='running'",
            params![
                run_id,
                finish.status,
                finish.finished_at,
                i64(finish.requests)?,
                i64(finish.reused)?,
                i64(finish.changed)?,
                i64(finish.failed_windows)?,
                i64(finish.unobserved_windows)?,
                finish.verified_through,
                finish.detail,
            ],
        )
        .map_err(storage)?;
    if changed != 1 {
        return Err(integrity(format!(
            "refresh run {run_id} was not open; refusing to finish it twice"
        )));
    }
    for source in sources {
        c.execute(
            "INSERT OR REPLACE INTO source_refresh_run_sources(\
                 run_id,source_id,class,complete,requests,snapshots,changed,\
                 unobserved_windows,failed_windows,verified_through) \
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                run_id,
                source.source_id,
                source.class,
                source.complete as i64,
                i64(source.requests)?,
                i64(source.snapshots)?,
                i64(source.changed)?,
                i64(source.unobserved_windows)?,
                i64(source.failed_windows)?,
                source.verified_through,
            ],
        )
        .map_err(storage)?;
    }
    for member in members {
        c.execute(
            "INSERT OR REPLACE INTO source_refresh_run_members(\
                 run_id,source_id,request_fingerprint,snapshot_id,page_count,\
                 row_count,as_of,fetched_at,changed,window_start,window_end) \
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                run_id,
                member.source_id,
                member.request_fingerprint,
                member.snapshot_id,
                member.page_count.map(i64::from),
                i64(member.row_count as u64)?,
                member.as_of,
                member.fetched_at,
                member.changed as i64,
                member.window_start,
                member.window_end,
            ],
        )
        .map_err(storage)?;
    }
    Ok(members.len())
}

pub(crate) fn read_runs(c: &Connection) -> Result<Vec<RunRow>, Error> {
    let mut statement = c
        .prepare(
            "SELECT run_id,mode,status,scope,started_at,finished_at,requests,reused,\
                    changed,failed_windows,unobserved_windows,verified_through,\
                    task_id,detail \
             FROM source_refresh_runs ORDER BY started_at, run_id",
        )
        .map_err(storage)?;
    let rows = statement
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, i64>(8)?,
                r.get::<_, i64>(9)?,
                r.get::<_, i64>(10)?,
                r.get::<_, Option<String>>(11)?,
                r.get::<_, Option<String>>(12)?,
                r.get::<_, Option<String>>(13)?,
            ))
        })
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    rows.into_iter()
        .map(|raw| {
            Ok(RunRow {
                run_id: raw.0,
                mode: raw.1,
                status: raw.2,
                scope: raw.3,
                started_at: raw.4,
                finished_at: raw.5,
                requests: u64(raw.6)?,
                reused: u64(raw.7)?,
                changed: u64(raw.8)?,
                failed_windows: u64(raw.9)?,
                unobserved_windows: u64(raw.10)?,
                verified_through: raw.11,
                task_id: raw.12,
                detail: raw.13,
            })
        })
        .collect()
}

pub(crate) fn read_sources(c: &Connection, run_id: &str) -> Result<Vec<RunSourceRow>, Error> {
    let mut statement = c
        .prepare(
            "SELECT run_id,source_id,class,complete,requests,snapshots,changed,\
                    unobserved_windows,failed_windows,verified_through \
             FROM source_refresh_run_sources WHERE run_id=?1 ORDER BY source_id",
        )
        .map_err(storage)?;
    let rows = statement
        .query_map(params![run_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, i64>(8)?,
                r.get::<_, Option<String>>(9)?,
            ))
        })
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    rows.into_iter()
        .map(|raw| {
            Ok(RunSourceRow {
                run_id: raw.0,
                source_id: raw.1,
                class: raw.2,
                complete: raw.3 != 0,
                requests: u64(raw.4)?,
                snapshots: u64(raw.5)?,
                changed: u64(raw.6)?,
                unobserved_windows: u64(raw.7)?,
                failed_windows: u64(raw.8)?,
                verified_through: raw.9,
            })
        })
        .collect()
}

pub(crate) fn read_members(c: &Connection, run_id: &str) -> Result<Vec<RunMemberRow>, Error> {
    let mut statement = c
        .prepare(
            "SELECT run_id,source_id,request_fingerprint,snapshot_id,page_count,\
                    row_count,as_of,fetched_at,changed,window_start,window_end \
             FROM source_refresh_run_members WHERE run_id=?1 \
             ORDER BY source_id, request_fingerprint",
        )
        .map_err(storage)?;
    let rows = statement
        .query_map(params![run_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<i64>>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, i64>(8)?,
                r.get::<_, Option<String>>(9)?,
                r.get::<_, Option<String>>(10)?,
            ))
        })
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    rows.into_iter()
        .map(|raw| {
            Ok(RunMemberRow {
                run_id: raw.0,
                source_id: raw.1,
                request_fingerprint: raw.2,
                snapshot_id: raw.3,
                page_count: match raw.4 {
                    Some(page_count) => Some(u32::try_from(page_count).map_err(|_| {
                        integrity(format!("refresh member page_count {page_count} is invalid"))
                    })?),
                    None => None,
                },
                row_count: usize::try_from(raw.5).map_err(|_| {
                    integrity(format!("refresh member row_count {} is invalid", raw.5))
                })?,
                as_of: raw.6,
                fetched_at: raw.7,
                changed: raw.8 != 0,
                window_start: raw.9,
                window_end: raw.10,
            })
        })
        .collect()
}

/// The newest date through which a *complete* successful run has confirmed
/// `source_id`. A tail that only reads two windows (or one probe page) never
/// appears here: it did not verify the fingerprint set the H=14-day gate is
/// about (issue-446 §5.1/§5.3).
pub(crate) fn latest_complete_verified_through(
    c: &Connection,
    source_id: &str,
) -> Result<Option<String>, Error> {
    c.query_row(
        "SELECT s.verified_through \
         FROM source_refresh_run_sources s \
         JOIN source_refresh_runs r ON r.run_id = s.run_id \
         WHERE s.source_id=?1 AND s.complete=1 AND r.status='succeeded' \
           AND s.verified_through IS NOT NULL \
         ORDER BY r.finished_at DESC, r.run_id DESC LIMIT 1",
        params![source_id],
        |r| r.get(0),
    )
    .optional()
    .map_err(storage)
}

fn i64(value: u64) -> Result<i64, Error> {
    i64::try_from(value).map_err(|_| {
        integrity(format!(
            "refresh counter {value} exceeds the SQLite integer range"
        ))
    })
}

fn u64(value: i64) -> Result<u64, Error> {
    u64::try_from(value).map_err(|_| integrity(format!("refresh counter {value} is negative")))
}
