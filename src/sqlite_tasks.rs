//! SQLite task adapter. Every operation owns one bounded transaction/snapshot.
use crate::task_control::*;
use chrono::Utc;
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    time::Duration,
};
pub const SCHEMA_VERSION: u32 = 10;
const MIGRATIONS: [&str; 10] = [
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
];
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
    let (fk, sync, mode): (i64, i64, String) = (
        c.query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .map_err(storage)?,
        c.query_row("PRAGMA synchronous", [], |r| r.get(0))
            .map_err(storage)?,
        c.query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .map_err(storage)?,
    );
    if fk != 1 || sync != 2 || mode != "wal" {
        return Err(Error::StorageUnavailable(
            "connection pragmas differ from task contract".into(),
        ));
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
fn read_task(c: &Connection, id: &str) -> Result<Option<TaskSnapshot>, Error> {
    let mut stmt=c.prepare("SELECT command_key_json,status,state_revision,accepted_run_id,owner_run_id,created_at,last_progress_at,frozen_work_json,result_json,retry_of FROM tasks WHERE task_id=?1").map_err(storage)?;
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
    let mut stmt=c.prepare("SELECT scope_key,task_id,state_revision FROM scope_occupancy WHERE task_id=?1 ORDER BY scope_key").map_err(storage)?;
    let claims = stmt
        .query_map([id], |r| {
            Ok(ScopeClaim {
                scope_key: r.get(0)?,
                task_id: r.get(1)?,
                state_revision: read_revision(r, 2)?,
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
    validate_snapshot(&s, &scopes)?;
    crate::sqlite_verification::validate_task_link(c, &s)?;
    crate::sqlite_market::publication_checks::validate_task_link(c, &s)?;
    Ok(Some(s))
}
fn validate_snapshot(s: &TaskSnapshot, scopes: &BTreeSet<String>) -> Result<(), Error> {
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
    let total = s.items.len();
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
pub(crate) fn write_task(c: &Connection, s: &TaskSnapshot, expected: u64) -> Result<(), Error> {
    let changed=c.execute("UPDATE tasks SET status=?1,state_revision=?2,owner_run_id=?3,last_progress_at=?4,result_json=?5 WHERE task_id=?6 AND state_revision=?7",params![tag(&s.task.status),db_revision(s.task.state_revision)?,s.task.owner_run_id,s.task.last_progress_at.to_rfc3339(),s.task.result.as_ref().map(encode).transpose()?,s.task.task_id,db_revision(expected)?]).map_err(storage)?;
    if changed != 1 {
        return Err(Error::RevisionConflict);
    }
    for i in &s.items {
        c.execute(
            "UPDATE task_items SET stage=?1,outcome_json=?2 WHERE task_id=?3 AND item_id=?4",
            params![
                tag(&i.stage),
                i.outcome.as_ref().map(encode).transpose()?,
                s.task.task_id,
                i.spec.item_id
            ],
        )
        .map_err(storage)?;
    }
    if s.task.status.terminal() {
        c.execute(
            "DELETE FROM scope_occupancy WHERE task_id=?1",
            [&s.task.task_id],
        )
        .map_err(storage)?;
    } else {
        c.execute(
            "UPDATE scope_occupancy SET state_revision=?1 WHERE task_id=?2",
            params![db_revision(s.task.state_revision)?, s.task.task_id],
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
        self.write(|c|{
            if let Some(record)=read_request(c,id)?{if record.command!=*command{return Err(Error::RequestConflict);}return Ok(Acceptance{record,reused:true});}
            // Corrupt accepted state is not permission to steal a missing claim.
            let orphan: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM tasks t LEFT JOIN requests r ON r.task_id=t.task_id WHERE r.request_id IS NULL)", [], |r| r.get(0)).map_err(storage)?;
            if orphan { return Err(inconsistent("unbound execution record")); }
            let mut stmt=c.prepare("SELECT request_id FROM requests").map_err(storage)?;
            let ids=stmt.query_map([],|r|r.get::<_,String>(0)).map_err(storage)?.collect::<Result<Vec<_>,_>>().map_err(storage)?;
            for existing in ids{read_request(c,&existing)?;}
            let execution_key=if let Some(guard)=retry {
                let old=read_bound_task(c,&guard.original_task_id)?.ok_or(Error::TaskNotFound)?;
                if old.task.state_revision!=guard.expected_revision{return Err(Error::RevisionConflict);}
                if command.intent!=serde_json::json!({"original_task_id":guard.original_task_id,"selected_items":guard.selected_items}) || command.key!= (CommandKey{operation:"retry_task".into(),..old.task.key.clone()}) {return Err(Error::InvalidRetrySelection);}
                if c.query_row("SELECT request_id FROM requests WHERE task_id=?1",[&guard.original_task_id],|r|r.get::<_,String>(0)).map_err(storage)?==id{return Err(Error::RequestConflict);}
                for item in &work.items {if !guard.selected_items.contains(&item.item_id) || !old.items.iter().any(|i|i.spec==*item && !i.outcome.as_ref().is_some_and(|o|o.success)){return Err(Error::InvalidRetrySelection);}}
                if work.items.len()!=guard.selected_items.len(){return Err(Error::InvalidRetrySelection);}
                old.task.key
            }else{command.key.clone()};
            let mut conflicts=Vec::new();
            for scope in &work.scopes {if c.query_row("SELECT EXISTS(SELECT 1 FROM scope_occupancy WHERE scope_key=?1)",[scope],|r|r.get::<_,bool>(0)).map_err(storage)?{conflicts.push(scope.clone());}}
            if !conflicts.is_empty(){return Err(Error::RangeConflict(conflicts));}
            let task_id:String=c.query_row("SELECT 'task-' || lower(hex(randomblob(16)))",[],|r|r.get(0)).map_err(storage)?;
            let now=Utc::now().to_rfc3339();
            c.execute("INSERT INTO tasks VALUES(?1,?2,'pending',0,?3,NULL,?4,?4,?5,NULL,?6)",params![task_id,encode(&execution_key)?,run,now,encode(work)?,retry.map(|g|&g.original_task_id)]).map_err(storage)?;
            for i in &work.items {c.execute("INSERT INTO task_items VALUES(?1,?2,?3,?4,?5,?6,'pending',NULL)",params![task_id,i.item_id,encode(&i.target)?,i.progress_unit,encode(&i.frozen_inputs)?,encode(&i.scope_keys)?]).map_err(storage)?;}
            for scope in &work.scopes {c.execute("INSERT INTO task_scopes VALUES(?1,?2)",params![task_id,scope]).map_err(storage)?;c.execute("INSERT INTO scope_occupancy VALUES(?1,?2,0)",params![scope,task_id]).map_err(storage)?;}
            let raw=encode(command)?;
            c.execute("INSERT INTO requests VALUES(?1,?2,?3,'task',?4,NULL)",params![id,raw,crate::hash::sha256(raw.as_bytes()),task_id]).map_err(storage)?;
            Ok(Acceptance{record:read_request(c,id)?.ok_or_else(||inconsistent("inserted binding missing"))?,reused:false})
        })
    }
    fn transition(
        &self,
        id: &str,
        revision: u64,
        run: &str,
        change: Change,
    ) -> Result<TaskSnapshot, Error> {
        self.write(|c| {
            let old = read_bound_task(c, id)?.ok_or(Error::TaskNotFound)?;
            if old.task.state_revision != revision {
                return Err(Error::RevisionConflict);
            }
            let started_item = match &change {
                Change::StartItem(item_id) => Some(item_id.clone()),
                _ => None,
            };
            let now = Utc::now();
            let next = decide(old, run, change, now)?;
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
        })
    }
    fn candidates(&self, run: &str) -> Result<Vec<TaskSnapshot>, Error> {
        self.read(|c|{
        let mut stmt=c.prepare("SELECT task_id FROM tasks WHERE accepted_run_id=?1 AND owner_run_id IS NULL AND status IN ('pending','cancel_requested') ORDER BY created_at,task_id").map_err(storage)?;
        let ids=stmt.query_map([run],|r|r.get::<_,String>(0)).map_err(storage)?.collect::<Result<Vec<_>,_>>().map_err(storage)?;
        ids.iter().map(|id|read_bound_task(c,id)?.ok_or(Error::TaskNotFound)).collect()
    })
    }
    fn reconcile(&self, run: &str) -> Result<usize, Error> {
        self.write(|c|{
        let mut stmt=c.prepare("SELECT task_id FROM tasks WHERE status NOT IN ('succeeded','partially_failed','failed','cancelled','interrupted') AND (accepted_run_id<>?1 OR (owner_run_id IS NOT NULL AND owner_run_id<>?1))").map_err(storage)?;
        let ids=stmt.query_map([run],|r|r.get::<_,String>(0)).map_err(storage)?.collect::<Result<Vec<_>,_>>().map_err(storage)?;
        let mut count=0;for id in ids{let old=read_bound_task(c,&id)?.ok_or(Error::TaskNotFound)?;let rev=old.task.state_revision;let next=decide(old,run,Change::Reconcile,Utc::now())?;if next.task.state_revision!=rev{write_task(c,&next,rev)?;count+=1;}}Ok(count)
    })
    }
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
