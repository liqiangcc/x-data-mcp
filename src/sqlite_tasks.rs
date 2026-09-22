//! SQLite task adapter. Every operation owns one bounded transaction/snapshot.
use crate::task_control::*;
use chrono::Utc;
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    time::Duration,
};
pub const SCHEMA_VERSION: u32 = 18;
pub(crate) const MIGRATIONS: [&str; 18] = [
    include_str!("../migrations/tasks/001.sql"),
    include_str!("../migrations/tasks/002.sql"),
    include_str!("../migrations/tasks/003_market.sql"),
    include_str!("../migrations/tasks/004_catalog.sql"),
    include_str!("../migrations/tasks/005_update_results.sql"),
    include_str!("../migrations/tasks/006_yearly_results.sql"),
    include_str!("../migrations/tasks/007_stopped_checks.sql"),
    include_str!("../migrations/tasks/008_publication_checks.sql"),
    include_str!("../migrations/tasks/009_verification_inspections.sql"),
    include_str!("../migrations/tasks/010_execution_starts.sql"),
    include_str!("../migrations/tasks/011_proven_gaps.sql"),
    include_str!("../migrations/tasks/012_gap_verification_decisions.sql"),
    include_str!("../migrations/tasks/013_r7_raw.sql"),
    include_str!("../migrations/tasks/014_segmented_bars.sql"),
    include_str!("../migrations/tasks/015_task_counters.sql"),
    include_str!("../migrations/tasks/016_compact_records.sql"),
    include_str!("../migrations/tasks/017_manifest_delta.sql"),
    include_str!("../migrations/tasks/018_shared_evidence.sql"),
];
/// Compact market storage (#259) is an explicit opt-in target: schema-19 files
/// are refused by `check_schema` above so legacy adapters stay fail-closed.
pub const COMPACT_SCHEMA_VERSION: u32 = 19;
pub(crate) const COMPACT_MIGRATION: &str =
    include_str!("../migrations/tasks/019_compact_market.sql");
/// Data migrations that cannot be expressed in SQL run inside the same
/// transaction immediately after their schema step.
fn post_migration(c: &Connection, version: usize) -> Result<(), Error> {
    match version {
        16 => crate::sqlite_market::envelope::migrate_legacy(c).map_err(|e| {
            Error::StorageUnavailable(format!("compact record migration failed: {e}"))
        }),
        17 => crate::sqlite_market::manifest::migrate(c)
            .map_err(|e| Error::StorageUnavailable(format!("manifest migration failed: {e}"))),
        _ => Ok(()),
    }
}
#[derive(Clone)]
pub struct SqliteTasks {
    path: PathBuf,
    busy: Duration,
}
impl SqliteTasks {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            busy: Duration::from_millis(250),
        }
    }
    pub fn with_busy_timeout(mut self, busy: Duration) -> Self {
        self.busy = busy;
        self
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// Durable schema version of the database file (0 = uninitialized,
    /// 18 = legacy market storage, 19 = compact market storage).
    pub fn schema_version(&self) -> Result<u32, Error> {
        let c = self.connect(false)?;
        c.query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(storage)
    }
    /// Truncate-checkpoint the WAL after a bounded batch of writes.
    /// Returns SQLite's (busy, log_frames, checkpointed_frames); a busy
    /// result is a fact for the caller to report, never silently ignored.
    pub fn checkpoint_truncate(&self) -> Result<(i64, i64, i64), Error> {
        let c = self.connect(false)?;
        c.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .map_err(storage)
    }
    /// Mechanical durable fact only: whether this item's StartItem transition
    /// committed a start marker. No contract or business filtering here.
    pub fn item_execution_started(&self, task_id: &str, item_id: &str) -> Result<bool, Error> {
        self.read(|c| {
            c.query_row(
                "SELECT EXISTS(SELECT 1 FROM task_item_execution_starts WHERE task_id=?1 AND item_id=?2)",
                params![task_id, item_id],
                |r| r.get(0),
            )
            .map_err(storage)
        })
    }
    pub fn initialize(&self) -> Result<(), Error> {
        self.initialize_target(false)
    }
    pub fn initialize19(&self) -> Result<(), Error> {
        self.initialize_target(true)
    }
    fn initialize_target(&self, compact: bool) -> Result<(), Error> {
        // Explicit create-new only. No clobber, migration or import of an existing file.
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&self.path)
            .map_err(storage)?;
        let mut c = Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(storage)?;
        c.busy_timeout(self.busy).map_err(storage)?;
        let mode: String = c
            .query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))
            .map_err(storage)?;
        if mode != "wal" {
            return Err(Error::StorageUnavailable("WAL unavailable".into()));
        }
        configure(&c, self.busy)?;
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        for (index, sql) in MIGRATIONS.iter().enumerate() {
            apply_migration(&tx, index + 1, sql)?;
            post_migration(&tx, index + 1)?;
        }
        if compact {
            apply_migration(&tx, COMPACT_SCHEMA_VERSION as usize, COMPACT_MIGRATION)?;
        }
        tx.commit()
            .map_err(|e| Error::OutcomeUnknown(e.to_string()))
    }
    pub fn upgrade(&self) -> Result<(), Error> {
        let mut c = self.connect(false)?;
        // SQLite table rebuild requires enforcement off OUTSIDE the transaction.
        // The connection is private to this explicit schema operation only.
        c.pragma_update(None, "foreign_keys", false)
            .map_err(storage)?;
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        let version = check_schema(&tx, false)?;
        for (index, sql) in MIGRATIONS.iter().enumerate().skip(version as usize) {
            apply_migration(&tx, index + 1, sql)?;
            post_migration(&tx, index + 1)?;
        }
        let broken: bool = tx
            .prepare("PRAGMA foreign_key_check")
            .map_err(storage)?
            .exists([])
            .map_err(storage)?;
        if broken {
            return Err(inconsistent("schema upgrade found foreign key violations"));
        }
        tx.commit()
            .map_err(|e| Error::OutcomeUnknown(e.to_string()))?;
        c.pragma_update(None, "foreign_keys", true)
            .map_err(storage)?;
        Ok(())
    }
    /// Explicit 18→19 compact storage upgrade. Every business table must be
    /// empty; a populated schema-18 database can only be replaced by a fresh
    /// rebuild, never migrated in place.
    pub fn upgrade_compact(&self) -> Result<(), Error> {
        let mut c = self.connect(false)?;
        c.pragma_update(None, "foreign_keys", false)
            .map_err(storage)?;
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        let version = check_schema(&tx, false)?;
        if version != SCHEMA_VERSION {
            return Err(Error::StorageUnavailable(format!(
                "schema {version} cannot upgrade to compact {COMPACT_SCHEMA_VERSION}; requires exact schema {SCHEMA_VERSION}"
            )));
        }
        require_business_tables_empty(&tx)?;
        apply_migration(&tx, COMPACT_SCHEMA_VERSION as usize, COMPACT_MIGRATION)?;
        let broken: bool = tx
            .prepare("PRAGMA foreign_key_check")
            .map_err(storage)?
            .exists([])
            .map_err(storage)?;
        if broken {
            return Err(inconsistent("schema upgrade found foreign key violations"));
        }
        tx.commit()
            .map_err(|e| Error::OutcomeUnknown(e.to_string()))?;
        c.pragma_update(None, "foreign_keys", true)
            .map_err(storage)?;
        Ok(())
    }
    pub(crate) fn connect(&self, readonly: bool) -> Result<Connection, Error> {
        let flag = if readonly {
            OpenFlags::SQLITE_OPEN_READ_ONLY
        } else {
            OpenFlags::SQLITE_OPEN_READ_WRITE
        };
        let c = Connection::open_with_flags(&self.path, flag).map_err(storage)?;
        configure(&c, self.busy)?;
        if readonly {
            c.pragma_update(None, "query_only", true).map_err(storage)?;
        }
        Ok(c)
    }
    pub(crate) fn read<T>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let mut c = self.connect(true)?;
        let tx = c.transaction().map_err(storage)?;
        check_schema(&tx, true)?;
        let value = f(&tx)?;
        tx.commit().map_err(storage)?;
        Ok(value)
    }
    pub(crate) fn write<T>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let mut c = self.connect(false)?;
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        check_schema(&tx, true)?;
        let value = f(&tx)?;
        #[cfg(test)]
        tests::crash_point("before_commit");
        tx.commit()
            .map_err(|e| Error::OutcomeUnknown(e.to_string()))?;
        #[cfg(test)]
        tests::crash_point("after_commit");
        Ok(value)
    }
}
fn configure(c: &Connection, busy: Duration) -> Result<(), Error> {
    c.busy_timeout(busy).map_err(storage)?;
    c.pragma_update(None, "foreign_keys", true)
        .map_err(storage)?;
    c.pragma_update(None, "synchronous", "FULL")
        .map_err(storage)?;
    // WAL space must stay bounded: autocheckpoint every ~4MiB of frames and
    // never retain a larger high-water WAL after checkpoint.
    c.pragma_update(None, "wal_autocheckpoint", 1000)
        .map_err(storage)?;
    c.pragma_update(None, "journal_size_limit", 16_777_216)
        .map_err(storage)?;
    let (fk, sync, mode, autocp, jsl): (i64, i64, String, i64, i64) = (
        c.query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .map_err(storage)?,
        c.query_row("PRAGMA synchronous", [], |r| r.get(0))
            .map_err(storage)?,
        c.query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .map_err(storage)?,
        c.query_row("PRAGMA wal_autocheckpoint", [], |r| r.get(0))
            .map_err(storage)?,
        c.query_row("PRAGMA journal_size_limit", [], |r| r.get(0))
            .map_err(storage)?,
    );
    if fk != 1 || sync != 2 || mode != "wal" || autocp != 1000 || jsl != 16_777_216 {
        return Err(Error::StorageUnavailable(
            "connection pragmas differ from task contract".into(),
        ));
    }
    Ok(())
}
fn require_business_tables_empty(c: &Connection) -> Result<(), Error> {
    let mut stmt = c
        .prepare(
            "SELECT name FROM sqlite_master WHERE type='table' \
             AND name<>'schema_migrations' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .map_err(storage)?;
    let names = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    drop(stmt);
    for name in names {
        let populated: bool = c
            .query_row(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM \"{}\" LIMIT 1)",
                    name.replace('"', "\"\"")
                ),
                [],
                |r| r.get(0),
            )
            .map_err(storage)?;
        if populated {
            return Err(Error::StorageUnavailable(
                "FRESH_REBUILD_REQUIRED: schema 19 compact market storage requires a new database"
                    .into(),
            ));
        }
    }
    Ok(())
}
fn apply_migration(c: &Connection, version: usize, sql: &str) -> Result<(), Error> {
    c.execute_batch(sql).map_err(storage)?;
    c.execute(
        "INSERT INTO schema_migrations VALUES(?1,?2,?3)",
        params![
            version as u32,
            crate::hash::sha256(sql.as_bytes()),
            Utc::now().to_rfc3339()
        ],
    )
    .map_err(storage)?;
    c.pragma_update(None, "user_version", version as u32)
        .map_err(storage)?;
    Ok(())
}
pub(crate) fn check_schema(c: &Connection, current: bool) -> Result<u32, Error> {
    let version: u32 = c
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(storage)?;
    if version == 0 || version > SCHEMA_VERSION || current && version != SCHEMA_VERSION {
        return Err(Error::StorageUnavailable(format!(
            "unsupported/uninitialized schema {version}; explicit initialization/upgrade required"
        )));
    }
    let mut stmt = c
        .prepare("SELECT version,checksum FROM schema_migrations ORDER BY version")
        .map_err(storage)?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, u32>(0)?, r.get::<_, String>(1)?)))
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    if rows.len() != version as usize
        || rows.iter().enumerate().any(|(i, (v, d))| {
            *v as usize != i + 1 || *d != crate::hash::sha256(MIGRATIONS[i].as_bytes())
        })
    {
        return Err(Error::StorageUnavailable(
            "schema migration identity mismatch".into(),
        ));
    }
    Ok(version)
}
fn encode<T: Serialize>(v: &T) -> Result<String, Error> {
    serde_json::to_string(v).map_err(storage)
}
fn decode<T: DeserializeOwned>(s: String) -> Result<T, Error> {
    serde_json::from_str(&s).map_err(|e| Error::NeedsVerification(e.to_string()))
}
fn tag<T: Serialize>(v: &T) -> String {
    serde_json::to_value(v).unwrap().as_str().unwrap().into()
}
fn storage(e: impl std::fmt::Display) -> Error {
    Error::StorageUnavailable(e.to_string())
}
fn inconsistent(s: &str) -> Error {
    Error::NeedsVerification(s.into())
}
#[derive(Default)]
struct ItemCounts {
    total: i64,
    pending: i64,
    running: i64,
    finished: i64,
    succeeded: i64,
}
fn read_task(c: &Connection, id: &str) -> Result<Option<TaskSnapshot>, Error> {
    let mut stmt=c.prepare("SELECT command_key_json,status,state_revision,accepted_run_id,owner_run_id,created_at,last_progress_at,frozen_work_json,result_json,retry_of,item_count,items_pending,items_running,items_finished,items_succeeded,evidence_digest FROM tasks WHERE task_id=?1").map_err(storage)?;
    let mut rows = stmt.query([id]).map_err(storage)?;
    let Some(r) = rows.next().map_err(storage)? else {
        return Ok(None);
    };
    let status: String = r.get(1).map_err(storage)?;
    let task = TaskRecord {
        task_id: id.into(),
        key: decode(r.get(0).map_err(storage)?)?,
        status: decode(format!("\"{status}\""))?,
        state_revision: read_revision(r, 2).map_err(storage)?,
        accepted_run_id: r.get(3).map_err(storage)?,
        owner_run_id: r.get(4).map_err(storage)?,
        created_at: r
            .get::<_, String>(5)
            .map_err(storage)?
            .parse()
            .map_err(storage)?,
        last_progress_at: r
            .get::<_, String>(6)
            .map_err(storage)?
            .parse()
            .map_err(storage)?,
        work: decode(r.get(7).map_err(storage)?)?,
        result: r
            .get::<_, Option<String>>(8)
            .map_err(storage)?
            .map(decode)
            .transpose()?,
        retry_of: r.get(9).map_err(storage)?,
    };
    // The head projection reads the digest column; the snapshot derives it
    // from the shared evidence blob. Divergence means one side was tampered
    // with — fail closed instead of letting the two read paths disagree.
    let column_digest: Option<String> = r.get(15).map_err(storage)?;
    if column_digest.as_deref() != task.work.evidence_digest() {
        return Err(inconsistent("task evidence digest column/blob mismatch"));
    }
    let counts = ItemCounts {
        total: r.get(10).map_err(storage)?,
        pending: r.get(11).map_err(storage)?,
        running: r.get(12).map_err(storage)?,
        finished: r.get(13).map_err(storage)?,
        succeeded: r.get(14).map_err(storage)?,
    };
    #[cfg(test)]
    tests::after_task_row();
    let mut stmt=c.prepare("SELECT item_id,target_json,progress_unit,frozen_inputs_json,scope_keys_json,stage,outcome_json FROM task_items WHERE task_id=?1 ORDER BY item_id").map_err(storage)?;
    let mut rows = stmt.query([id]).map_err(storage)?;
    let mut items = Vec::new();
    while let Some(r) = rows.next().map_err(storage)? {
        let stage: String = r.get(5).map_err(storage)?;
        items.push(TaskItem {
            spec: ItemSpec {
                item_id: r.get(0).map_err(storage)?,
                target: decode(r.get(1).map_err(storage)?)?,
                progress_unit: r.get(2).map_err(storage)?,
                frozen_inputs: decode(r.get(3).map_err(storage)?)?,
                scope_keys: decode(r.get(4).map_err(storage)?)?,
            },
            stage: decode(format!("\"{stage}\""))?,
            outcome: r
                .get::<_, Option<String>>(6)
                .map_err(storage)?
                .map(decode)
                .transpose()?,
        });
    }
    // Occupancy rows are a static scope→task binding; the claim's revision
    // is the task's current revision, maintained on the task row.
    let mut stmt = c
        .prepare(
            "SELECT scope_key,task_id FROM scope_occupancy WHERE task_id=?1 ORDER BY scope_key",
        )
        .map_err(storage)?;
    let claims = stmt
        .query_map([id], |r| {
            Ok(ScopeClaim {
                scope_key: r.get(0)?,
                task_id: r.get(1)?,
                state_revision: task.state_revision,
            })
        })
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    let mut stmt = c
        .prepare("SELECT scope_key FROM task_scopes WHERE task_id=?1 ORDER BY scope_key")
        .map_err(storage)?;
    let scopes = stmt
        .query_map([id], |r| r.get::<_, String>(0))
        .map_err(storage)?
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(storage)?;
    let s = TaskSnapshot {
        task,
        items,
        claims,
    };
    validate_snapshot(&s, &scopes, &counts)?;
    crate::sqlite_verification::validate_task_link(c, &s)?;
    crate::sqlite_market::publication_checks::validate_task_link(c, &s)?;
    Ok(Some(s))
}
fn validate_snapshot(
    s: &TaskSnapshot,
    scopes: &BTreeSet<String>,
    counts: &ItemCounts,
) -> Result<(), Error> {
    let uncertainty = s.execution_uncertainty()?;
    s.task
        .work
        .validate()
        .map_err(|e| inconsistent(&e.to_string()))?;
    if s.task.accepted_run_id.trim().is_empty()
        || &s.task.work.scopes != scopes
        || s.items.len() != s.task.work.items.len()
    {
        return Err(inconsistent("task/frozen work incomplete"));
    }
    for i in &s.items {
        if !s.task.work.items.contains(&i.spec)
            || (i.stage == Stage::Finished) != i.outcome.is_some()
            || i.outcome
                .as_ref()
                .is_some_and(|o| o.success && o.error.is_some())
        {
            return Err(inconsistent("item evidence inconsistent"));
        }
    }
    if s.task.status == Status::Running && s.task.owner_run_id.is_none()
        || s.task.status == Status::Pending
            && (s.task.owner_run_id.is_some() || s.items.iter().any(|i| i.stage != Stage::Pending))
    {
        return Err(inconsistent(
            "execution owner/stage missing or inconsistent",
        ));
    }
    if !s.task.status.terminal() {
        if s.claims
            .iter()
            .map(|c| c.scope_key.clone())
            .collect::<BTreeSet<_>>()
            != *scopes
            || s.claims
                .iter()
                .any(|c| c.state_revision != s.task.state_revision || c.task_id != s.task.task_id)
        {
            return Err(inconsistent("active occupancy missing/inconsistent"));
        }
    } else if !s.claims.is_empty()
        || s.task.result.is_none()
        || s.items.iter().any(|i| i.stage == Stage::Running)
    {
        return Err(inconsistent("terminal execution/claims inconsistent"));
    }
    let finished = s
        .items
        .iter()
        .filter(|i| i.stage == Stage::Finished)
        .count();
    let success = s
        .items
        .iter()
        .filter(|i| i.outcome.as_ref().is_some_and(|o| o.success))
        .count();
    let running = s.items.iter().filter(|i| i.stage == Stage::Running).count();
    let total = s.items.len();
    if counts.total != total as i64
        || counts.pending != (total - finished - running) as i64
        || counts.running != running as i64
        || counts.finished != finished as i64
        || counts.succeeded != success as i64
    {
        return Err(inconsistent("task item counters diverged from items"));
    }
    let valid = match s.task.status {
        Status::Succeeded => finished == total && success == total,
        Status::Failed => finished == total && success == 0,
        Status::PartiallyFailed => finished == total && success > 0 && success < total,
        Status::Interrupted => s
            .task
            .result
            .as_ref()
            .and_then(|r| r.get("interruption"))
            .is_some(),
        Status::Cancelled => {
            finished < total
                || (success < total
                    && s.task
                        .result
                        .as_ref()
                        .and_then(|r| r.get("cancellation_acknowledged"))
                        == Some(&serde_json::Value::Bool(true)))
        }
        Status::NeedsVerification => s.task.result.is_none() || uncertainty.is_some(),
        _ => s.task.result.is_none(),
    };
    if !valid
        || s.task
            .owner_run_id
            .as_ref()
            .is_some_and(|r| r.trim().is_empty())
        || (s.task.owner_run_id.is_none() && s.items.iter().any(|i| i.stage != Stage::Pending))
    {
        return Err(inconsistent("execution status/outcomes inconsistent"));
    }
    Ok(())
}
struct RequestRow {
    command: String,
    digest: String,
    kind: String,
    task_id: Option<String>,
    terminal: Option<String>,
}
pub(crate) fn read_request(c: &Connection, id: &str) -> Result<Option<RequestRecord>, Error> {
    let row = c.query_row(
        "SELECT command_json,command_digest,outcome_kind,task_id,terminal_json FROM requests WHERE request_id=?1",
        [id],
        |r| Ok(RequestRow {command:r.get(0)?,digest:r.get(1)?,kind:r.get(2)?,task_id:r.get(3)?,terminal:r.get(4)?}),
    ).optional().map_err(storage)?;
    let Some(RequestRow {
        command: raw,
        digest,
        kind,
        task_id,
        terminal,
    }) = row
    else {
        #[cfg(target_os = "linux")]
        crate::sqlite_verification_inspection::validate_absent_request(c, id)?;
        return Ok(None);
    };
    if crate::hash::sha256(raw.as_bytes()) != digest {
        return Err(inconsistent("command digest mismatch"));
    }
    let command: Command = decode(raw)?;
    let outcome = match (kind.as_str(), task_id, terminal) {
        ("task", Some(bound_task_id), None) => {
            #[cfg(target_os = "linux")]
            crate::sqlite_verification_inspection::validate_task_request(c, id, &command)?;
            let s = read_task(c, &bound_task_id)?
                .ok_or_else(|| inconsistent("bound execution record missing"))?;
            let expected = if s.task.retry_of.is_some() {
                CommandKey {
                    operation: "retry_task".into(),
                    ..s.task.key.clone()
                }
            } else {
                s.task.key.clone()
            };
            if command.key != expected {
                return Err(inconsistent("request/task contract mismatch"));
            }
            RequestOutcome::Task(Box::new(s))
        }
        ("terminal", None, Some(v)) => {
            let value = decode(v)?;
            crate::sqlite_verification::validate_terminal(c, &command, &value)?;
            #[cfg(target_os = "linux")]
            crate::sqlite_verification_inspection::validate_terminal(c, id, &command, &value)?;
            crate::sqlite_market::publication_checks::validate_terminal(c, &command, &value)?;
            RequestOutcome::Terminal(value)
        }
        _ => return Err(inconsistent("request outcome incomplete")),
    };
    Ok(Some(RequestRecord {
        request_id: id.into(),
        command,
        outcome,
    }))
}
pub(crate) fn read_bound_task(c: &Connection, id: &str) -> Result<Option<TaskSnapshot>, Error> {
    let snapshot = read_task(c, id)?;
    let binding: Option<String> = c
        .query_row(
            "SELECT request_id FROM requests WHERE task_id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()
        .map_err(storage)?;
    match (snapshot, binding) {
        (Some(s), Some(request)) => {
            read_request(c, &request)?;
            Ok(Some(s))
        }
        (None, None) => Ok(None),
        _ => Err(inconsistent("task binding/execution missing")),
    }
}
/// O(1) task-row projection: every field the runner/commit guards need,
/// with no frozen-work or item-list decode.
pub(crate) fn read_task_head(c: &Connection, id: &str) -> Result<Option<TaskHead>, Error> {
    let mut stmt = c.prepare("SELECT command_key_json,status,state_revision,accepted_run_id,owner_run_id,last_progress_at,result_json,item_count,items_pending,items_running,items_finished,items_succeeded,evidence_digest FROM tasks WHERE task_id=?1").map_err(storage)?;
    let mut rows = stmt.query([id]).map_err(storage)?;
    let Some(r) = rows.next().map_err(storage)? else {
        return Ok(None);
    };
    let status: String = r.get(1).map_err(storage)?;
    Ok(Some(TaskHead {
        task_id: id.into(),
        key: decode(r.get(0).map_err(storage)?)?,
        status: decode(format!("\"{status}\""))?,
        state_revision: read_revision(r, 2).map_err(storage)?,
        accepted_run_id: r.get(3).map_err(storage)?,
        owner_run_id: r.get(4).map_err(storage)?,
        last_progress_at: r
            .get::<_, String>(5)
            .map_err(storage)?
            .parse()
            .map_err(storage)?,
        result: r
            .get::<_, Option<String>>(6)
            .map_err(storage)?
            .map(decode)
            .transpose()?,
        counts: crate::task_control::ItemCounts {
            total: r.get::<_, i64>(7).map_err(storage)? as u64,
            pending: r.get::<_, i64>(8).map_err(storage)? as u64,
            running: r.get::<_, i64>(9).map_err(storage)? as u64,
            finished: r.get::<_, i64>(10).map_err(storage)? as u64,
            succeeded: r.get::<_, i64>(11).map_err(storage)? as u64,
        },
        evidence_digest: r.get(12).map_err(storage)?,
    }))
}
fn read_task_item(c: &Connection, task_id: &str, item_id: &str) -> Result<Option<TaskItem>, Error> {
    let mut stmt = c.prepare("SELECT item_id,target_json,progress_unit,frozen_inputs_json,scope_keys_json,stage,outcome_json FROM task_items WHERE task_id=?1 AND item_id=?2").map_err(storage)?;
    let mut rows = stmt.query(params![task_id, item_id]).map_err(storage)?;
    let Some(r) = rows.next().map_err(storage)? else {
        return Ok(None);
    };
    let stage: String = r.get(5).map_err(storage)?;
    Ok(Some(TaskItem {
        spec: ItemSpec {
            item_id: r.get(0).map_err(storage)?,
            target: decode(r.get(1).map_err(storage)?)?,
            progress_unit: r.get(2).map_err(storage)?,
            frozen_inputs: decode(r.get(3).map_err(storage)?)?,
            scope_keys: decode(r.get(4).map_err(storage)?)?,
        },
        stage: decode(format!("\"{stage}\""))?,
        outcome: r
            .get::<_, Option<String>>(6)
            .map_err(storage)?
            .map(decode)
            .transpose()?,
    }))
}
/// One task row plus one item row plus the occupancy coverage fact — the
/// complete O(1) pre-image for per-item execution and commit binding.
/// `None` means the task or the item row is absent.
pub(crate) fn read_item_binding(
    c: &Connection,
    task_id: &str,
    item_id: &str,
) -> Result<Option<ItemBinding>, Error> {
    let (Some(head), Some(item)) = (
        read_task_head(c, task_id)?,
        read_task_item(c, task_id, item_id)?,
    ) else {
        return Ok(None);
    };
    // A `selection_evidence` marker must equal the task's shared-evidence
    // digest; a mismatched marker — or one on a task carrying no shared
    // evidence — is a tampered/corrupt row and fails closed.
    if let Some(marker) = item
        .spec
        .frozen_inputs
        .get("selection_evidence")
        .and_then(|v| v.get("evidence_digest"))
        .and_then(Value::as_str)
        && head.evidence_digest.as_deref() != Some(marker)
    {
        return Err(inconsistent(
            "item evidence marker mismatches task shared evidence",
        ));
    }
    let bound: i64 = c
        .query_row(
            "SELECT count(*) FROM scope_occupancy WHERE task_id=?1 AND scope_key IN (SELECT value FROM json_each(?2))",
            params![task_id, encode(&item.spec.scope_keys)?],
            |r| r.get(0),
        )
        .map_err(storage)?;
    Ok(Some(ItemBinding {
        head,
        scopes_bound: bound > 0 && bound as usize == item.spec.scope_keys.len(),
        item,
    }))
}
/// Persist a `decide_head`-decided binding: the touched item row (when the
/// change carried one), the guarded task row with matching counter deltas,
/// and the single terminal occupancy release. `expected` is the pre-decision
/// revision.
pub(crate) fn persist_decided(
    c: &Connection,
    head: &TaskHead,
    item: Option<&TaskItem>,
    expected: u64,
) -> Result<(), Error> {
    // Item rows are written before the guarded task-row update so a counter
    // recount observes the post-transition state.
    if let Some(item) = item {
        write_item_row(c, &head.task_id, item)?;
    }
    let counter = counter_delta(item.is_some(), item.and_then(|i| i.outcome.as_ref()));
    guarded_task_update(c, &TaskRow::from(head), counter, expected)?;
    if head.status.terminal() {
        #[cfg(test)]
        tests::count_write("scope_occupancy");
        c.execute(
            "DELETE FROM scope_occupancy WHERE task_id=?1",
            [&head.task_id],
        )
        .map_err(storage)?;
    }
    Ok(())
}
/// O(1) transition on a caller-held head+item pair: verifies the persisted
/// revision (refreshing the pair in place when stale), applies `decide_head`,
/// and persists only the touched rows. Equivalent to `transition_light` for
/// callers that never materialize the full item vector.
pub(crate) fn transition_head_tx(
    c: &Connection,
    head: &mut TaskHead,
    mut item: Option<&mut TaskItem>,
    run: &str,
    change: Change,
    now: chrono::DateTime<Utc>,
) -> Result<(), Error> {
    let persisted: Option<i64> = c
        .query_row(
            "SELECT state_revision FROM tasks WHERE task_id=?1",
            [&head.task_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(storage)?;
    match persisted {
        None => return Err(Error::TaskNotFound),
        Some(revision) if revision == db_revision(head.state_revision)? => {}
        Some(_) => {
            *head = read_task_head(c, &head.task_id)?.ok_or(Error::TaskNotFound)?;
            if let Some(item) = item.as_deref_mut() {
                let item_id = item.spec.item_id.clone();
                *item = read_task_item(c, &head.task_id, &item_id)?
                    .ok_or_else(|| inconsistent("bound item missing"))?;
            }
        }
    }
    let old_revision = head.state_revision;
    // StartItem/FinishItem rewrite the item row; RequireVerification binds
    // the item but leaves its row at `running`.
    let writes_item = matches!(change, Change::StartItem(_) | Change::FinishItem(..));
    let started_item = match &change {
        Change::StartItem(item_id) => Some(item_id.clone()),
        _ => None,
    };
    decide_head(head, item.as_deref_mut(), run, change, now)?;
    if head.state_revision == old_revision {
        return Ok(());
    }
    persist_decided(
        c,
        head,
        if writes_item { item.as_deref() } else { None },
        old_revision,
    )?;
    if let Some(started_item) = started_item {
        // Execution-start marker lands atomically with the guarded
        // item transition.
        c.execute(
            "INSERT INTO task_item_execution_starts VALUES(?1,?2,?3,?4,?5)",
            params![
                head.task_id,
                started_item,
                run,
                db_revision(head.state_revision)?,
                now.to_rfc3339()
            ],
        )
        .map_err(storage)?;
    }
    Ok(())
}
fn write_items_delta(c: &Connection, s: &TaskSnapshot, changed: Option<&str>) -> Result<(), Error> {
    match changed {
        Some(id) => {
            let item = s
                .items
                .iter()
                .find(|i| i.spec.item_id == id)
                .ok_or_else(|| inconsistent("changed item absent from task"))?;
            write_item_row(c, &s.task.task_id, item)?;
        }
        None => {
            // Diff against persisted rows so a whole-snapshot write only
            // updates items that actually changed.
            let mut stmt = c
                .prepare("SELECT item_id,stage,outcome_json FROM task_items WHERE task_id=?1")
                .map_err(storage)?;
            let persisted = stmt
                .query_map([&s.task.task_id], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        (r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?),
                    ))
                })
                .map_err(storage)?
                .collect::<Result<std::collections::HashMap<String, _>, _>>()
                .map_err(storage)?;
            for item in &s.items {
                let outcome = item.outcome.as_ref().map(encode).transpose()?;
                match persisted.get(&item.spec.item_id) {
                    Some((stage, old)) if *stage == tag(&item.stage) && *old == outcome => {}
                    _ => write_item_row(c, &s.task.task_id, item)?,
                }
            }
        }
    }
    Ok(())
}
fn write_item_row(c: &Connection, task_id: &str, item: &TaskItem) -> Result<(), Error> {
    #[cfg(test)]
    tests::count_write("task_items");
    c.execute(
        "UPDATE task_items SET stage=?1,outcome_json=?2 WHERE task_id=?3 AND item_id=?4",
        params![
            tag(&item.stage),
            item.outcome.as_ref().map(encode).transpose()?,
            task_id,
            item.spec.item_id
        ],
    )
    .map_err(storage)?;
    Ok(())
}
/// The task-row fields a decided transition writes; shared shape of the
/// snapshot and head persistence paths.
struct TaskRow<'a> {
    task_id: &'a str,
    status: Status,
    revision: u64,
    owner: &'a Option<String>,
    progress: &'a chrono::DateTime<Utc>,
    result: &'a Option<serde_json::Value>,
}
impl<'a> From<&'a TaskHead> for TaskRow<'a> {
    fn from(head: &'a TaskHead) -> Self {
        Self {
            task_id: &head.task_id,
            status: head.status,
            revision: head.state_revision,
            owner: &head.owner_run_id,
            progress: &head.last_progress_at,
            result: &head.result,
        }
    }
}
/// Guarded task-row update shared by the snapshot and head paths.
/// `counter` is the item-counter SQL fragment: incremental deltas for
/// single-item transitions, a recount for whole-snapshot writes, empty for
/// changes that cannot alter any item stage.
fn guarded_task_update(
    c: &Connection,
    row: &TaskRow<'_>,
    counter: &str,
    expected: u64,
) -> Result<(), Error> {
    let sql = format!(
        "UPDATE tasks SET status=?1,state_revision=?2,owner_run_id=?3,last_progress_at=?4,result_json=?5{counter} WHERE task_id=?6 AND state_revision=?7"
    );
    #[cfg(test)]
    tests::count_write("tasks");
    let changed_rows = c
        .execute(
            &sql,
            params![
                tag(&row.status),
                db_revision(row.revision)?,
                row.owner,
                row.progress.to_rfc3339(),
                row.result.as_ref().map(encode).transpose()?,
                row.task_id,
                db_revision(expected)?
            ],
        )
        .map_err(storage)?;
    if changed_rows != 1 {
        return Err(Error::RevisionConflict);
    }
    Ok(())
}
/// Counter SQL fragment matching the in-memory arithmetic of a decided
/// change: `Some(item_id)` + no outcome is a start, an outcome is a finish;
/// `None` means the change could not touch any item.
fn counter_delta(changed: bool, outcome: Option<&ItemOutcome>) -> &'static str {
    match (changed, outcome) {
        (true, None) => ",items_pending=items_pending-1,items_running=items_running+1",
        (true, Some(o)) if o.success => {
            ",items_running=items_running-1,items_finished=items_finished+1,items_succeeded=items_succeeded+1"
        }
        (true, Some(_)) => ",items_running=items_running-1,items_finished=items_finished+1",
        (false, _) => "",
    }
}
const RECOUNT_COUNTERS: &str = ",item_count=(SELECT count(*) FROM task_items i WHERE i.task_id=tasks.task_id),items_pending=(SELECT count(*) FROM task_items i WHERE i.task_id=tasks.task_id AND i.stage='pending'),items_running=(SELECT count(*) FROM task_items i WHERE i.task_id=tasks.task_id AND i.stage='running'),items_finished=(SELECT count(*) FROM task_items i WHERE i.task_id=tasks.task_id AND i.stage='finished'),items_succeeded=(SELECT count(*) FROM task_items i WHERE i.task_id=tasks.task_id AND i.stage='finished' AND json_extract(i.outcome_json,'$.success')=1)";
fn update_task_row(
    c: &Connection,
    s: &TaskSnapshot,
    expected: u64,
    changed: Option<&str>,
    outcome: Option<&ItemOutcome>,
) -> Result<(), Error> {
    // Item counters are maintained either incrementally (single-item
    // transition) or by recount (whole-snapshot write).
    let counter = if changed.is_none() {
        RECOUNT_COUNTERS
    } else {
        counter_delta(true, outcome)
    };
    guarded_task_update(
        c,
        &TaskRow {
            task_id: &s.task.task_id,
            status: s.task.status,
            revision: s.task.state_revision,
            owner: &s.task.owner_run_id,
            progress: &s.task.last_progress_at,
            result: &s.task.result,
        },
        counter,
        expected,
    )
}
pub(crate) fn write_task(c: &Connection, s: &TaskSnapshot, expected: u64) -> Result<(), Error> {
    write_task_delta(c, s, expected, None, None)
}
/// Persist `next` (the post-transition snapshot). `changed`/`outcome`
/// identify the single item this transition touched, allowing O(1) writes;
/// `None` diffs persisted rows and recounts counters instead.
pub(crate) fn write_task_delta(
    c: &Connection,
    s: &TaskSnapshot,
    expected: u64,
    changed: Option<&str>,
    outcome: Option<&ItemOutcome>,
) -> Result<(), Error> {
    // Item rows are written before the guarded task-row update so a
    // counter recount observes the post-transition state.
    write_items_delta(c, s, changed)?;
    update_task_row(c, s, expected, changed, outcome)?;
    if s.task.status.terminal() {
        #[cfg(test)]
        tests::count_write("scope_occupancy");
        c.execute(
            "DELETE FROM scope_occupancy WHERE task_id=?1",
            [&s.task.task_id],
        )
        .map_err(storage)?;
    }
    Ok(())
}
impl TaskRepository for SqliteTasks {
    fn read_request(&self, id: &str) -> Result<Option<RequestRecord>, Error> {
        self.read(|c| read_request(c, id))
    }
    fn read_task(&self, id: &str) -> Result<Option<TaskSnapshot>, Error> {
        self.read(|c| read_bound_task(c, id))
    }

    fn accept(
        &self,
        id: &str,
        command: &Command,
        work: &FrozenWork,
        run: &str,
        retry: Option<&RetryGuard>,
    ) -> Result<Acceptance, Error> {
        work.validate()?;
        if id.trim().is_empty() || run.trim().is_empty() {
            return Err(Error::InvalidCommand("empty identity".into()));
        }
        self.write(|c| accept_tx(c, id, command, work, run, retry))
    }
    fn transition(
        &self,
        id: &str,
        revision: u64,
        run: &str,
        change: Change,
    ) -> Result<TaskSnapshot, Error> {
        self.write(|c| transition_tx(c, id, revision, run, change))
    }
    fn candidates(&self, run: &str) -> Result<Vec<TaskSnapshot>, Error> {
        self.read(|c| candidates_tx(c, run))
    }
    fn reconcile(&self, run: &str) -> Result<usize, Error> {
        self.write(|c| reconcile_tx(c, run))
    }
    /// O(1) per-item transition: verifies only the task-row revision (which
    /// serializes every mutation), decides on the caller-held snapshot in
    /// place, and writes just the task row plus the one touched item row.
    /// A stale pre-image is refreshed once — equivalent to the caller
    /// re-reading before `transition` — and `decide` guards still reject
    /// illegal changes on the actual state.
    fn transition_light(
        &self,
        s: &mut TaskSnapshot,
        run: &str,
        change: Change,
    ) -> Result<(), Error> {
        self.write(|c| transition_light_tx(c, s, run, change))
    }
}

/// Connection-level accept used by both `SqliteTasks` and the schema-19
/// compact repository; semantics are identical.
pub(crate) fn accept_tx(
    c: &Connection,
    id: &str,
    command: &Command,
    work: &FrozenWork,
    run: &str,
    retry: Option<&RetryGuard>,
) -> Result<Acceptance, Error> {
    if let Some(record) = read_request(c, id)? {
        if record.command != *command {
            return Err(Error::RequestConflict);
        }
        return Ok(Acceptance {
            record,
            reused: true,
        });
    }
    // Corrupt accepted state is not permission to steal a missing claim.
    let orphan: bool = c
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM tasks t LEFT JOIN requests r ON r.task_id=t.task_id WHERE r.request_id IS NULL)",
            [],
            |r| r.get(0),
        )
        .map_err(storage)?;
    if orphan {
        return Err(inconsistent("unbound execution record"));
    }
    let mut stmt = c
        .prepare("SELECT request_id FROM requests")
        .map_err(storage)?;
    let ids = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    drop(stmt);
    for existing in ids {
        read_request(c, &existing)?;
    }
    let execution_key = if let Some(guard) = retry {
        let old = read_bound_task(c, &guard.original_task_id)?.ok_or(Error::TaskNotFound)?;
        if old.task.state_revision != guard.expected_revision {
            return Err(Error::RevisionConflict);
        }
        if command.intent
            != serde_json::json!({"original_task_id":guard.original_task_id,"selected_items":guard.selected_items})
            || command.key
                != (CommandKey {
                    operation: "retry_task".into(),
                    ..old.task.key.clone()
                })
        {
            return Err(Error::InvalidRetrySelection);
        }
        if c.query_row(
            "SELECT request_id FROM requests WHERE task_id=?1",
            [&guard.original_task_id],
            |r| r.get::<_, String>(0),
        )
        .map_err(storage)?
            == id
        {
            return Err(Error::RequestConflict);
        }
        for item in &work.items {
            if !guard.selected_items.contains(&item.item_id)
                || !old
                    .items
                    .iter()
                    .any(|i| i.spec == *item && !i.outcome.as_ref().is_some_and(|o| o.success))
            {
                return Err(Error::InvalidRetrySelection);
            }
        }
        if work.items.len() != guard.selected_items.len() {
            return Err(Error::InvalidRetrySelection);
        }
        old.task.key
    } else {
        command.key.clone()
    };
    let mut conflicts = Vec::new();
    for scope in &work.scopes {
        if c.query_row(
            "SELECT EXISTS(SELECT 1 FROM scope_occupancy WHERE scope_key=?1)",
            [scope],
            |r| r.get::<_, bool>(0),
        )
        .map_err(storage)?
        {
            conflicts.push(scope.clone());
        }
    }
    if !conflicts.is_empty() {
        return Err(Error::RangeConflict(conflicts));
    }
    let task_id: String = c
        .query_row("SELECT 'task-' || lower(hex(randomblob(16)))", [], |r| {
            r.get(0)
        })
        .map_err(storage)?;
    let now = Utc::now().to_rfc3339();
    c.execute(
        "INSERT INTO tasks(task_id,command_key_json,status,state_revision,accepted_run_id,owner_run_id,created_at,last_progress_at,frozen_work_json,result_json,retry_of,item_count,items_pending,evidence_digest) VALUES(?1,?2,'pending',0,?3,NULL,?4,?4,?5,NULL,?6,?7,?7,?8)",
        params![
            task_id,
            encode(&execution_key)?,
            run,
            now,
            encode(work)?,
            retry.map(|g| &g.original_task_id),
            work.items.len() as u32,
            work.evidence_digest()
        ],
    )
    .map_err(storage)?;
    for i in &work.items {
        c.execute(
            "INSERT INTO task_items VALUES(?1,?2,?3,?4,?5,?6,'pending',NULL)",
            params![
                task_id,
                i.item_id,
                encode(&i.target)?,
                i.progress_unit,
                encode(&i.frozen_inputs)?,
                encode(&i.scope_keys)?
            ],
        )
        .map_err(storage)?;
    }
    for scope in &work.scopes {
        c.execute(
            "INSERT INTO task_scopes VALUES(?1,?2)",
            params![task_id, scope],
        )
        .map_err(storage)?;
        c.execute(
            "INSERT INTO scope_occupancy VALUES(?1,?2,0)",
            params![scope, task_id],
        )
        .map_err(storage)?;
    }
    let raw = encode(command)?;
    c.execute(
        "INSERT INTO requests VALUES(?1,?2,?3,'task',?4,NULL)",
        params![id, raw, crate::hash::sha256(raw.as_bytes()), task_id],
    )
    .map_err(storage)?;
    Ok(Acceptance {
        record: read_request(c, id)?.ok_or_else(|| inconsistent("inserted binding missing"))?,
        reused: false,
    })
}

/// Connection-level transition shared by `SqliteTasks` and the compact
/// repository; identical decide/persist semantics.
pub(crate) fn transition_tx(
    c: &Connection,
    id: &str,
    revision: u64,
    run: &str,
    change: Change,
) -> Result<TaskSnapshot, Error> {
    let old = read_bound_task(c, id)?.ok_or(Error::TaskNotFound)?;
    if old.task.state_revision != revision {
        return Err(Error::RevisionConflict);
    }
    let started_item = match &change {
        Change::StartItem(item_id) => Some(item_id.clone()),
        _ => None,
    };
    let now = Utc::now();
    let mut next = old;
    decide(&mut next, run, change, now)?;
    if next.task.state_revision != revision {
        write_task(c, &next, revision)?;
        if let Some(item_id) = started_item {
            c.execute(
                "INSERT INTO task_item_execution_starts VALUES(?1,?2,?3,?4,?5)",
                params![
                    id,
                    item_id,
                    run,
                    db_revision(next.task.state_revision)?,
                    now.to_rfc3339()
                ],
            )
            .map_err(storage)?;
        }
    }
    read_bound_task(c, id)?.ok_or(Error::TaskNotFound)
}

pub(crate) fn candidates_tx(c: &Connection, run: &str) -> Result<Vec<TaskSnapshot>, Error> {
    let mut stmt = c
        .prepare("SELECT task_id FROM tasks WHERE accepted_run_id=?1 AND owner_run_id IS NULL AND status IN ('pending','cancel_requested') ORDER BY created_at,task_id")
        .map_err(storage)?;
    let ids = stmt
        .query_map([run], |r| r.get::<_, String>(0))
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    drop(stmt);
    ids.iter()
        .map(|id| read_bound_task(c, id)?.ok_or(Error::TaskNotFound))
        .collect()
}

pub(crate) fn reconcile_tx(c: &Connection, run: &str) -> Result<usize, Error> {
    let mut stmt = c
        .prepare("SELECT task_id FROM tasks WHERE status NOT IN ('succeeded','partially_failed','failed','cancelled','interrupted') AND (accepted_run_id<>?1 OR (owner_run_id IS NOT NULL AND owner_run_id<>?1))")
        .map_err(storage)?;
    let ids = stmt
        .query_map([run], |r| r.get::<_, String>(0))
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    drop(stmt);
    let mut count = 0;
    for id in ids {
        let mut next = read_bound_task(c, &id)?.ok_or(Error::TaskNotFound)?;
        let rev = next.task.state_revision;
        decide(&mut next, run, Change::Reconcile, Utc::now())?;
        if next.task.state_revision != rev {
            write_task(c, &next, rev)?;
            count += 1;
        }
    }
    Ok(count)
}

pub(crate) fn transition_light_tx(
    c: &Connection,
    s: &mut TaskSnapshot,
    run: &str,
    change: Change,
) -> Result<(), Error> {
    let persisted: Option<i64> = c
        .query_row(
            "SELECT state_revision FROM tasks WHERE task_id=?1",
            [&s.task.task_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(storage)?;
    match persisted {
        None => return Err(Error::TaskNotFound),
        // Revision serializes all task/item/occupancy writes, so the
        // caller's snapshot is the exact persisted pre-image.
        Some(revision) if revision == db_revision(s.task.state_revision)? => {}
        Some(_) => *s = read_bound_task(c, &s.task.task_id)?.ok_or(Error::TaskNotFound)?,
    }
    let old_revision = s.task.state_revision;
    let now = Utc::now();
    let (changed_id, outcome) = match &change {
        Change::StartItem(id) => (Some(id.clone()), None),
        Change::FinishItem(id, outcome) => (Some(id.clone()), Some(outcome.clone())),
        _ => (None, None),
    };
    let changed = changed_id.as_deref();
    let outcome = outcome.as_ref();
    decide(s, run, change, now)?;
    if s.task.state_revision != old_revision {
        write_task_delta(c, s, old_revision, changed, outcome)?;
        if let Some(item_id) = changed.filter(|_| outcome.is_none()) {
            // Execution-start marker lands atomically with the
            // guarded item transition.
            c.execute(
                "INSERT INTO task_item_execution_starts VALUES(?1,?2,?3,?4,?5)",
                params![
                    s.task.task_id,
                    item_id,
                    run,
                    db_revision(s.task.state_revision)?,
                    now.to_rfc3339()
                ],
            )
            .map_err(storage)?;
        }
    }
    Ok(())
}

fn read_revision(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let n: i64 = row.get(index)?;
    u64::try_from(n).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, n))
}
fn db_revision(n: u64) -> Result<i64, Error> {
    i64::try_from(n).map_err(|_| Error::RevisionConflict)
}

#[cfg(test)]
mod tests;
