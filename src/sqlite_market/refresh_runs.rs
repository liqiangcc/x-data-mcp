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
//! Refresh is single-flight (§5.6 R2b): [`begin_if_idle`] checks for a
//! `running` row in the same write transaction as its insert, and
//! [`reclaim_foreign`] closes rows a previous process instance left `running`.
//! Both decisions use the recorded `owner_instance`, never elapsed time.
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
    /// The service instance opening the run (`data_service`'s process run id).
    /// Startup reclaim closes only runs whose owner differs from the instance
    /// now holding the database lease.
    pub owner_instance: String,
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
    pub owner_instance: Option<String>,
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
    /// Wall clock of this source's acquisition in milliseconds, from its first
    /// request to its last one. G2's per-table gate reads this column (D-67).
    pub elapsed_ms: u64,
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

/// Open a run unless one is already `running` (§5.6 R2b: refresh is
/// single-flight, so two clients never share the exchanges' 1 request/s
/// limit). `Ok(Some(run_id))` rejects the start and names the open run; the
/// check and the insert are one write transaction, so two concurrent callers
/// cannot both see an idle record.
///
/// `tail` and `full` exclude each other by construction: the guard is per run
/// record, not per mode.
pub(crate) fn begin_if_idle(c: &Connection, start: &RunStart) -> Result<Option<String>, Error> {
    let running = c
        .query_row(
            "SELECT run_id FROM source_refresh_runs WHERE status='running' \
             ORDER BY started_at, run_id LIMIT 1",
            [],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(storage)?;
    if running.is_some() {
        return Ok(running);
    }
    c.execute(
        "INSERT INTO source_refresh_runs(\
             run_id,mode,status,scope,started_at,task_id,owner_instance) \
         VALUES(?1,?2,'running',?3,?4,?5,?6)",
        params![
            start.run_id,
            start.mode,
            start.scope,
            start.started_at,
            start.task_id,
            start.owner_instance,
        ],
    )
    .map_err(storage)?;
    Ok(None)
}

/// Close every `running` row whose owner is not `instance` (a `NULL` owner
/// predates the column and counts as foreign). The rows were opened by a
/// process that no longer holds the database lease, so they can never finish:
/// marking them `failed` with a `process_restarted` reason is the only honest
/// terminal state. `verified_through` is left untouched — and because the run
/// never finished, no source row exists to advance it.
///
/// Returns the reclaimed run ids, oldest first.
pub(crate) fn reclaim_foreign(
    c: &Connection,
    instance: &str,
    finished_at: &str,
    detail: &str,
) -> Result<Vec<String>, Error> {
    let ids = {
        let mut statement = c
            .prepare(
                "SELECT run_id FROM source_refresh_runs WHERE status='running' \
                   AND (owner_instance IS NULL OR owner_instance<>?1) \
                 ORDER BY started_at, run_id",
            )
            .map_err(storage)?;
        statement
            .query_map(params![instance], |r| r.get::<_, String>(0))
            .map_err(storage)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage)?
    };
    for run_id in &ids {
        let changed = c
            .execute(
                "UPDATE source_refresh_runs SET status='failed',finished_at=?2,detail=?3 \
                 WHERE run_id=?1 AND status='running'",
                params![run_id, finished_at, detail],
            )
            .map_err(storage)?;
        if changed != 1 {
            return Err(integrity(format!(
                "refresh run {run_id} left the running state while being reclaimed"
            )));
        }
    }
    Ok(ids)
}

/// Close one open run `failed` with a run-level reason. Used when the
/// acquisition cannot even reach per-request accounting (a store, transport or
/// database fault outside the loop). `Ok(false)` means the row was no longer
/// `running`; the caller reports its own error either way.
pub(crate) fn fail(
    c: &Connection,
    run_id: &str,
    finished_at: &str,
    detail: &str,
) -> Result<bool, Error> {
    let changed = c
        .execute(
            "UPDATE source_refresh_runs SET status='failed',finished_at=?2,detail=?3 \
             WHERE run_id=?1 AND status='running'",
            params![run_id, finished_at, detail],
        )
        .map_err(storage)?;
    Ok(changed == 1)
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
                 unobserved_windows,failed_windows,elapsed_ms,verified_through) \
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
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
                i64(source.elapsed_ms)?,
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

const RUN_COLUMNS: &str = "run_id,mode,status,scope,started_at,finished_at,requests,reused,\
     changed,failed_windows,unobserved_windows,verified_through,task_id,owner_instance,detail";

type RawRun = (
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    i64,
    i64,
    i64,
    i64,
    i64,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

fn raw_run(r: &rusqlite::Row<'_>) -> rusqlite::Result<RawRun> {
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
        r.get(10)?,
        r.get(11)?,
        r.get(12)?,
        r.get(13)?,
        r.get(14)?,
    ))
}

fn run_row(raw: RawRun) -> Result<RunRow, Error> {
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
        owner_instance: raw.13,
        detail: raw.14,
    })
}

pub(crate) fn read_runs(c: &Connection) -> Result<Vec<RunRow>, Error> {
    let mut statement = c
        .prepare(&format!(
            "SELECT {RUN_COLUMNS} FROM source_refresh_runs ORDER BY started_at, run_id"
        ))
        .map_err(storage)?;
    let rows = statement
        .query_map([], raw_run)
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    rows.into_iter().map(run_row).collect()
}

/// One durable run record, for `refresh_exchange_sources`'s `status_of`
/// lookup and for startup reclaim evidence.
pub(crate) fn read_run(c: &Connection, run_id: &str) -> Result<Option<RunRow>, Error> {
    c.query_row(
        &format!("SELECT {RUN_COLUMNS} FROM source_refresh_runs WHERE run_id=?1"),
        params![run_id],
        raw_run,
    )
    .optional()
    .map_err(storage)?
    .map(run_row)
    .transpose()
}

pub(crate) fn read_sources(c: &Connection, run_id: &str) -> Result<Vec<RunSourceRow>, Error> {
    let mut statement = c
        .prepare(
            "SELECT run_id,source_id,class,complete,requests,snapshots,changed,\
                    unobserved_windows,failed_windows,verified_through,elapsed_ms \
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
                r.get::<_, i64>(10)?,
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
                elapsed_ms: u64(raw.10)?,
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
        .query_map(params![run_id], member_row)
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    rows.into_iter().map(member_from_raw).collect()
}

/// Raw column tuple of one `source_refresh_run_members` row.
type RawMember = (
    String,
    String,
    String,
    String,
    Option<i64>,
    i64,
    String,
    String,
    i64,
    Option<String>,
    Option<String>,
);

fn member_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<RawMember> {
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
        r.get(10)?,
    ))
}

fn member_from_raw(raw: RawMember) -> Result<RunMemberRow, Error> {
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
        row_count: usize::try_from(raw.5)
            .map_err(|_| integrity(format!("refresh member row_count {} is invalid", raw.5)))?,
        as_of: raw.6,
        fetched_at: raw.7,
        changed: raw.8 != 0,
        window_start: raw.9,
        window_end: raw.10,
    })
}

/// The newest date through which a *complete* successful run has confirmed
/// `source_id`. A tail that only reads two windows (or one probe page) never
/// appears here: it did not verify the fingerprint set the H=14-day gate is
/// about (issue-446 §5.1/§5.3).
pub(crate) fn latest_complete_verified_through(
    c: &Connection,
    source_id: &str,
) -> Result<Option<String>, Error> {
    Ok(latest_complete(c, source_id)?.map(|run| run.verified_through))
}

/// One complete successful refresh of one source: the run identity R3 puts in a
/// derived item's evidence, the date the H gate compares, and the member pages
/// the run validated as one set (issue-446 §5.4/§5.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompleteRun {
    pub run_id: String,
    pub verified_through: String,
    pub members: Vec<RunMemberRow>,
}

/// The newest complete successful run of `source_id`, with the snapshot pages
/// it selected.
///
/// A whole-table set is only complete as *one acquisition*: the index cannot
/// say which page 1 belongs with which page 2, so the run record is the owner
/// of that identity (§5.4). Rows are returned in the run's own order.
pub(crate) fn latest_complete(
    c: &Connection,
    source_id: &str,
) -> Result<Option<CompleteRun>, Error> {
    let run = c
        .query_row(
            "SELECT r.run_id, s.verified_through \
             FROM source_refresh_run_sources s \
             JOIN source_refresh_runs r ON r.run_id = s.run_id \
             WHERE s.source_id=?1 AND s.complete=1 AND r.status='succeeded' \
               AND s.verified_through IS NOT NULL \
             ORDER BY r.finished_at DESC, r.run_id DESC LIMIT 1",
            params![source_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(storage)?;
    let Some((run_id, verified_through)) = run else {
        return Ok(None);
    };
    let members = read_members_for_source(c, &run_id, source_id)?;
    Ok(Some(CompleteRun {
        run_id,
        verified_through,
        members,
    }))
}

pub(crate) fn read_members_for_source(
    c: &Connection,
    run_id: &str,
    source_id: &str,
) -> Result<Vec<RunMemberRow>, Error> {
    let mut statement = c
        .prepare(
            "SELECT run_id,source_id,request_fingerprint,snapshot_id,page_count,\
                    row_count,as_of,fetched_at,changed,window_start,window_end \
             FROM source_refresh_run_members WHERE run_id=?1 AND source_id=?2 \
             ORDER BY request_fingerprint",
        )
        .map_err(storage)?;
    let rows = statement
        .query_map(params![run_id, source_id], member_row)
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    rows.into_iter().map(member_from_raw).collect()
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
