//! Durable acquisition state (D3): SQLite, not a mutable JSON file.
//!
//! Every mutating call carries a caller `request_id`; the request row is the
//! idempotency registry and `request_outcomes` binds the terminal result.
//! The single asynchronous unit of work is an acquisition task; every other
//! row is a fact that survives process death and can be reconciled
//! mechanically at open.
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};
use std::path::{Path, PathBuf};

const SCHEMA: &str = "
CREATE TABLE requests (
  request_id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  payload TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE TABLE request_outcomes (
  request_id TEXT PRIMARY KEY REFERENCES requests(request_id),
  outcome TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE TABLE acquisition_tasks (
  task_id TEXT PRIMARY KEY,
  request_id TEXT NOT NULL UNIQUE REFERENCES requests(request_id),
  kind TEXT NOT NULL,
  state TEXT NOT NULL,
  generation_id TEXT,
  stage TEXT,
  bytes_done INTEGER NOT NULL DEFAULT 0,
  error TEXT,
  detail TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE generations (
  generation_id TEXT PRIMARY KEY,
  state TEXT NOT NULL,
  manifest_sha256 TEXT NOT NULL,
  consistency_status TEXT,
  consistency_reasons TEXT,
  decision_sha256 TEXT,
  evidence_json TEXT,
  acquisition_request_id TEXT,
  installed_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE seen (
  role TEXT PRIMARY KEY,
  etag TEXT,
  last_modified TEXT,
  content_length INTEGER,
  seen_at TEXT NOT NULL
);
";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("acquire store unavailable: {0}")]
    Storage(String),
    #[error("request_id already bound to a different command")]
    RequestConflict,
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub request_id: String,
    pub kind: String,
    pub payload: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub task_id: String,
    pub request_id: String,
    pub kind: String,
    pub state: String,
    pub generation_id: Option<String>,
    pub stage: Option<String>,
    pub bytes_done: u64,
    pub error: Option<String>,
    pub detail: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generation {
    pub generation_id: String,
    pub state: String,
    pub manifest_sha256: String,
    pub consistency_status: Option<String>,
    pub consistency_reasons: Option<String>,
    pub decision_sha256: Option<String>,
    pub evidence_json: Option<String>,
    pub acquisition_request_id: Option<String>,
    pub installed_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub role: String,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub content_length: Option<u64>,
    pub seen_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct Db {
    path: PathBuf,
}
impl Db {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
    /// Create-or-open: the acquire database is owned by this service and
    /// initialised idempotently (IF NOT EXISTS schema, no migration story
    /// yet — schema v1 is the first version).
    pub fn open(&self) -> Result<(), Error> {
        let c = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )
        .map_err(storage)?;
        c.busy_timeout(std::time::Duration::from_millis(250))
            .map_err(storage)?;
        c.execute_batch(SCHEMA).map_err(storage)?;
        Ok(())
    }
    /// Read-only consumer check: the data service may observe recorded
    /// generation state but never creates or writes the acquire store.
    /// A missing database is an absent store, not an error.
    pub fn exists_readable(&self) -> Result<bool, Error> {
        if !self.path.exists() {
            return Ok(false);
        }
        let c = self.connect()?;
        c.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='generations'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map_err(storage)
        .map(|n| n == 1)
    }
    fn connect(&self) -> Result<Connection, Error> {
        let c = Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(storage)?;
        c.busy_timeout(std::time::Duration::from_millis(250))
            .map_err(storage)?;
        Ok(c)
    }
    fn write<T>(&self, f: impl FnOnce(&Connection) -> Result<T, Error>) -> Result<T, Error> {
        let c = self.connect()?;
        f(&c)
    }
    fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T, Error>) -> Result<T, Error> {
        let c = self.connect()?;
        f(&c)
    }

    /// Insert the request row once; replaying the same id with the same
    /// kind+payload returns the existing row, a different payload conflicts.
    pub fn accept_request(
        &self,
        request_id: &str,
        kind: &str,
        payload: &str,
        now: DateTime<Utc>,
    ) -> Result<(Request, bool), Error> {
        self.write(|c| {
            if let Some(existing) = request_row(c, request_id)? {
                if existing.kind == kind && existing.payload == payload {
                    return Ok((existing, true));
                }
                return Err(Error::RequestConflict);
            }
            c.execute(
                "INSERT INTO requests(request_id,kind,payload,created_at) VALUES(?1,?2,?3,?4)",
                params![request_id, kind, payload, now.to_rfc3339()],
            )
            .map_err(storage)?;
            Ok((
                Request {
                    request_id: request_id.into(),
                    kind: kind.into(),
                    payload: payload.into(),
                    created_at: now,
                },
                false,
            ))
        })
    }
    pub fn request(&self, request_id: &str) -> Result<Option<Request>, Error> {
        self.read(|c| request_row(c, request_id))
    }
    pub fn bind_outcome(
        &self,
        request_id: &str,
        outcome: &str,
        now: DateTime<Utc>,
    ) -> Result<(), Error> {
        self.write(|c| {
            c.execute(
                "INSERT OR IGNORE INTO request_outcomes(request_id,outcome,created_at) VALUES(?1,?2,?3)",
                params![request_id, outcome, now.to_rfc3339()],
            )
            .map_err(storage)?;
            Ok(())
        })
    }
    pub fn outcome<T: DeserializeOwned>(&self, request_id: &str) -> Result<Option<T>, Error> {
        self.read(|c| {
            let outcome: Option<String> = c
                .query_row(
                    "SELECT outcome FROM request_outcomes WHERE request_id=?1",
                    params![request_id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(storage)?;
            outcome
                .map(|o| serde_json::from_str(&o).map_err(Error::Json))
                .transpose()
        })
    }
    pub fn record_outcome<T: Serialize>(
        &self,
        request_id: &str,
        outcome: &T,
        now: DateTime<Utc>,
    ) -> Result<(), Error> {
        self.bind_outcome(request_id, &serde_json::to_string(outcome)?, now)
    }

    pub fn create_task(&self, task: &Task) -> Result<(), Error> {
        self.write(|c| {
            c.execute(
                "INSERT INTO acquisition_tasks(task_id,request_id,kind,state,generation_id,stage,bytes_done,error,detail,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                params![task.task_id, task.request_id, task.kind, task.state, task.generation_id, task.stage, task.bytes_done as i64, task.error, task.detail, task.created_at.to_rfc3339(), task.updated_at.to_rfc3339()],
            )
            .map_err(storage)?;
            Ok(())
        })
    }
    pub fn task(&self, task_id: &str) -> Result<Option<Task>, Error> {
        self.read(|c| task_row(c, "task_id", task_id))
    }
    pub fn task_by_request(&self, request_id: &str) -> Result<Option<Task>, Error> {
        self.read(|c| task_row(c, "request_id", request_id))
    }
    pub fn tasks_in(&self, states: &[&str]) -> Result<Vec<Task>, Error> {
        self.read(|c| {
            let marks = states.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!("SELECT task_id,request_id,kind,state,generation_id,stage,bytes_done,error,detail,created_at,updated_at FROM acquisition_tasks WHERE state IN ({marks}) ORDER BY created_at");
            let mut stmt = c.prepare(&sql).map_err(storage)?;
            let rows = stmt
                .query_map(rusqlite::params_from_iter(states.iter()), map_task)
                .map_err(storage)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(storage)
        })
    }
    pub fn update_task(
        &self,
        task_id: &str,
        state: &str,
        generation_id: Option<&str>,
        stage: Option<&str>,
        bytes_done: u64,
        error: Option<&str>,
        detail: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<(), Error> {
        self.write(|c| {
            c.execute(
                "UPDATE acquisition_tasks SET state=?2,generation_id=?3,stage=?4,bytes_done=?5,error=?6,detail=?7,updated_at=?8 WHERE task_id=?1",
                params![task_id, state, generation_id, stage, bytes_done as i64, error, detail, now.to_rfc3339()],
            )
            .map_err(storage)?;
            Ok(())
        })
    }

    pub fn upsert_generation(&self, generation: &Generation) -> Result<(), Error> {
        self.write(|c| {
            c.execute(
                "INSERT INTO generations(generation_id,state,manifest_sha256,consistency_status,consistency_reasons,decision_sha256,evidence_json,acquisition_request_id,installed_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) ON CONFLICT(generation_id) DO UPDATE SET state=?2,manifest_sha256=?3,consistency_status=?4,consistency_reasons=?5,decision_sha256=?6,evidence_json=?7,acquisition_request_id=?8,installed_at=?9,updated_at=?10",
                params![generation.generation_id, generation.state, generation.manifest_sha256, generation.consistency_status, generation.consistency_reasons, generation.decision_sha256, generation.evidence_json, generation.acquisition_request_id, generation.installed_at.to_rfc3339(), generation.updated_at.to_rfc3339()],
            )
            .map_err(storage)?;
            Ok(())
        })
    }
    pub fn generation(&self, generation_id: &str) -> Result<Option<Generation>, Error> {
        self.read(|c| {
            c.query_row(
                "SELECT generation_id,state,manifest_sha256,consistency_status,consistency_reasons,decision_sha256,evidence_json,acquisition_request_id,installed_at,updated_at FROM generations WHERE generation_id=?1",
                params![generation_id],
                map_generation,
            )
            .optional()
            .map_err(storage)
        })
    }
    /// Rows whose state is one of `states`; the durable lifecycle facts
    /// the service reconciles against the filesystem.
    pub fn generations_in(&self, states: &[&str]) -> Result<Vec<Generation>, Error> {
        self.read(|c| {
            let marks = states.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let mut stmt = c
                .prepare(&format!("SELECT generation_id,state,manifest_sha256,consistency_status,consistency_reasons,decision_sha256,evidence_json,acquisition_request_id,installed_at,updated_at FROM generations WHERE state IN ({marks}) ORDER BY installed_at"))
                .map_err(storage)?;
            let rows = stmt
                .query_map(rusqlite::params_from_iter(states.iter()), map_generation)
                .map_err(storage)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(storage)
        })
    }
    pub fn generations(&self) -> Result<Vec<Generation>, Error> {
        self.read(|c| {
            let mut stmt = c
                .prepare("SELECT generation_id,state,manifest_sha256,consistency_status,consistency_reasons,decision_sha256,evidence_json,acquisition_request_id,installed_at,updated_at FROM generations ORDER BY installed_at")
                .map_err(storage)?;
            let rows = stmt.query_map([], map_generation).map_err(storage)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(storage)
        })
    }

    pub fn seen(&self, role: &str) -> Result<Option<Seen>, Error> {
        self.read(|c| {
            c.query_row(
                "SELECT role,etag,last_modified,content_length,seen_at FROM seen WHERE role=?1",
                params![role],
                |r| {
                    Ok(Seen {
                        role: r.get(0)?,
                        etag: r.get(1)?,
                        last_modified: r.get(2)?,
                        content_length: r
                            .get::<_, Option<i64>>(3)?
                            .map(|v| v.max(0) as u64),
                        seen_at: parse_time(r.get::<_, String>(4)?),
                    })
                },
            )
            .optional()
            .map_err(storage)
        })
    }
    pub fn record_seen(&self, seen: &Seen) -> Result<(), Error> {
        self.write(|c| {
            c.execute(
                "INSERT INTO seen(role,etag,last_modified,content_length,seen_at) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(role) DO UPDATE SET etag=?2,last_modified=?3,content_length=?4,seen_at=?5",
                params![seen.role, seen.etag, seen.last_modified, seen.content_length.map(|v| v as i64), seen.seen_at.to_rfc3339()],
            )
            .map_err(storage)?;
            Ok(())
        })
    }
}

fn storage(e: rusqlite::Error) -> Error {
    Error::Storage(e.to_string())
}
fn parse_time(s: String) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(&s)
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_else(|_| DateTime::<Utc>::UNIX_EPOCH)
}
fn request_row(c: &Connection, request_id: &str) -> Result<Option<Request>, Error> {
    c.query_row(
        "SELECT request_id,kind,payload,created_at FROM requests WHERE request_id=?1",
        params![request_id],
        |r| {
            Ok(Request {
                request_id: r.get(0)?,
                kind: r.get(1)?,
                payload: r.get(2)?,
                created_at: parse_time(r.get::<_, String>(3)?),
            })
        },
    )
    .optional()
    .map_err(storage)
}
fn task_row(c: &Connection, column: &str, key: &str) -> Result<Option<Task>, Error> {
    c.query_row(
        &format!("SELECT task_id,request_id,kind,state,generation_id,stage,bytes_done,error,detail,created_at,updated_at FROM acquisition_tasks WHERE {column}=?1"),
        params![key],
        map_task,
    )
    .optional()
    .map_err(storage)
}
fn map_task(r: &rusqlite::Row<'_>) -> rusqlite::Result<Task> {
    Ok(Task {
        task_id: r.get(0)?,
        request_id: r.get(1)?,
        kind: r.get(2)?,
        state: r.get(3)?,
        generation_id: r.get(4)?,
        stage: r.get(5)?,
        bytes_done: r.get::<_, i64>(6)?.max(0) as u64,
        error: r.get(7)?,
        detail: r.get(8)?,
        created_at: parse_time(r.get::<_, String>(9)?),
        updated_at: parse_time(r.get::<_, String>(10)?),
    })
}
fn map_generation(r: &rusqlite::Row<'_>) -> rusqlite::Result<Generation> {
    Ok(Generation {
        generation_id: r.get(0)?,
        state: r.get(1)?,
        manifest_sha256: r.get(2)?,
        consistency_status: r.get(3)?,
        consistency_reasons: r.get(4)?,
        decision_sha256: r.get(5)?,
        evidence_json: r.get(6)?,
        acquisition_request_id: r.get(7)?,
        installed_at: parse_time(r.get::<_, String>(8)?),
        updated_at: parse_time(r.get::<_, String>(9)?),
    })
}
