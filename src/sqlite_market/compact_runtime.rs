use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};
use sha2::{Digest as _, Sha256};

use super::*;
use crate::compact_market::Digest;
use crate::local_runtime::DatabaseLease;

// Test-only count of coordinator read transactions on this thread.
#[cfg(test)]
thread_local! {
    pub(crate) static READ_TXS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

const MAX_READERS: usize = 16;
const JOURNAL_SIZE_LIMIT: i64 = 16_777_216;
const WRITER_CACHE: usize = 128;
const READER_CACHE: usize = 32;
const IDENTITY_DOMAIN: &[u8] = b"x-data-schema-identity-v1\0";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SchemaIdentity {
    pub user_version: u32,
    pub digest: Digest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CheckpointMode {
    Passive,
    Truncate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CheckpointMetrics {
    pub mode: CheckpointMode,
    pub busy: i64,
    pub log_frames: i64,
    pub checkpointed_frames: i64,
    pub wal_bytes_before: u64,
    pub wal_bytes_after: u64,
}

fn push_text(out: &mut Vec<u8>, value: &str) {
    out.extend_from_slice(&(value.len() as u32).to_be_bytes());
    out.extend_from_slice(value.as_bytes());
}

fn unavailable(error: impl std::fmt::Display) -> Error {
    Error::Base(error.to_string())
}

pub(crate) fn inspect_schema_identity(c: &Connection) -> Result<SchemaIdentity, Error> {
    let raw_version: i64 = c
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(storage)?;
    if raw_version <= 0 || raw_version > u32::MAX as i64 {
        return Err(integrity("schema user_version out of range"));
    }
    let user_version = raw_version as u32;
    let mut statement = c
        .prepare("SELECT version,checksum FROM schema_migrations ORDER BY version")
        .map_err(storage)?;
    let rows = statement
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    drop(statement);
    if rows.len() != user_version as usize {
        return Err(integrity(
            "schema migration count differs from user_version",
        ));
    }
    let mut payload = IDENTITY_DOMAIN.to_vec();
    payload.extend_from_slice(&user_version.to_be_bytes());
    payload.extend_from_slice(&(rows.len() as u32).to_be_bytes());
    for (index, (version, checksum)) in rows.iter().enumerate() {
        if *version != index as i64 + 1 || checksum.is_empty() {
            return Err(integrity("schema migration sequence invalid"));
        }
        payload.extend_from_slice(&(*version as u32).to_be_bytes());
        push_text(&mut payload, checksum);
    }
    Ok(SchemaIdentity {
        user_version,
        digest: Sha256::digest(&payload).into(),
    })
}

struct ReaderSlot<'a> {
    count: &'a Mutex<usize>,
    idle: &'a Condvar,
}

impl<'a> ReaderSlot<'a> {
    fn acquire(count: &'a Mutex<usize>, idle: &'a Condvar) -> Result<Self, Error> {
        let mut active = count
            .lock()
            .map_err(|_| unavailable("reader slots poisoned"))?;
        while *active >= MAX_READERS {
            active = idle
                .wait(active)
                .map_err(|_| unavailable("reader slots poisoned"))?;
        }
        *active += 1;
        Ok(Self { count, idle })
    }
}

impl Drop for ReaderSlot<'_> {
    fn drop(&mut self) {
        if let Ok(mut active) = self.count.lock() {
            *active -= 1;
            self.idle.notify_all();
        }
    }
}

pub(crate) struct WriteCoordinator {
    path: PathBuf,
    lease: Arc<DatabaseLease>,
    expected: SchemaIdentity,
    busy: Duration,
    lifecycle: RwLock<()>,
    writer: Mutex<Option<Connection>>,
    reader_count: Mutex<usize>,
    reader_idle: Condvar,
    readers: Mutex<Vec<Connection>>,
}

impl WriteCoordinator {
    pub(crate) fn open_existing(
        path: impl AsRef<Path>,
        lease: Arc<DatabaseLease>,
        expected: SchemaIdentity,
        busy: Duration,
    ) -> Result<Self, Error> {
        let path = path.as_ref();
        if path != lease.path() {
            return Err(unavailable("coordinator path does not match lease"));
        }
        lease.validate().map_err(unavailable)?;
        let writer = open_writer(path, busy)?;
        let identity = inspect_schema_identity(&writer)?;
        if identity != expected {
            return Err(integrity("schema identity does not match expected"));
        }
        Ok(Self {
            path: path.to_path_buf(),
            lease,
            expected,
            busy,
            lifecycle: RwLock::new(()),
            writer: Mutex::new(Some(writer)),
            reader_count: Mutex::new(0),
            reader_idle: Condvar::new(),
            readers: Mutex::new(Vec::new()),
        })
    }

    fn verify_boundary(&self, conn: &Connection) -> Result<(), Error> {
        self.lease.validate().map_err(unavailable)?;
        if inspect_schema_identity(conn)? != self.expected {
            return Err(integrity("schema identity changed"));
        }
        Ok(())
    }

    pub(crate) fn write<T>(
        &self,
        f: impl FnOnce(&mut Connection) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let lock_span = crate::stage_trace::span("coord.write.lock_wait");
        let _lifecycle = self
            .lifecycle
            .read()
            .map_err(|_| unavailable("lifecycle lock poisoned"))?;
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| unavailable("writer lock poisoned"))?;
        drop(lock_span);
        let Some(conn) = writer.as_mut() else {
            return Err(unavailable("coordinator is shut down"));
        };
        crate::stage_trace::timed("coord.write.boundary_pre", || self.verify_boundary(conn))?;
        if !conn.is_autocommit() {
            return Err(integrity("writer connection is not in autocommit"));
        }
        let _held = crate::stage_trace::span("coord.write.held");
        // XP-041: no digest memo inside a write transaction.
        let _no_digest_memo = super::compact_versions::suspend_cache();
        let wal_before = crate::stage_trace::enabled().then(|| self.wal_bytes());
        let outcome = f(conn);
        if let Some(before) = wal_before {
            crate::stage_trace::note(
                "coord.write.wal_bytes",
                &[("before", before), ("after", self.wal_bytes())],
            );
        }
        match outcome {
            Err(error) => match recover_leak(conn) {
                Ok(()) => Err(error),
                Err(fatal) => Err(fatal),
            },
            Ok(value) => {
                if !conn.is_autocommit() {
                    recover_leak(conn)?;
                    return Err(integrity("write closure leaked a transaction"));
                }
                crate::stage_trace::timed("coord.write.boundary_post", || {
                    self.verify_boundary(conn)
                })?;
                Ok(value)
            }
        }
    }

    pub(crate) fn transaction<T>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T, Error>,
    ) -> Result<T, Error> {
        self.write(|conn| {
            let tx = crate::stage_trace::timed("coord.tx.begin", || {
                conn.transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(storage)
            })?;
            let value = crate::stage_trace::timed("coord.tx.closure", || f(&tx))?;
            crate::stage_trace::timed("coord.tx.commit", || {
                tx.commit()
                    .map_err(|error| Error::OutcomeUnknown(error.to_string()))
            })?;
            Ok(value)
        })
    }

    /// Pops a configured read-only connection from the pool or opens one.
    /// Readers are bounded by `ReaderSlot` (MAX_READERS), so the pool can
    /// never exceed that count.
    fn take_reader(&self) -> Result<Connection, Error> {
        if let Some(conn) = self
            .readers
            .lock()
            .map_err(|_| unavailable("reader pool poisoned"))?
            .pop()
        {
            return Ok(conn);
        }
        let conn = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(storage)?;
        conn.busy_timeout(self.busy).map_err(storage)?;
        set_pragma(&conn, "foreign_keys", 1)?;
        set_pragma(&conn, "query_only", 1)?;
        conn.set_prepared_statement_cache_capacity(READER_CACHE);
        verify_wal(&conn)?;
        Ok(conn)
    }

    /// Returns a connection to the pool. A connection that is not back in
    /// autocommit (committed or rolled back) is dropped instead of pooled.
    fn release_reader(&self, conn: Connection) {
        if !conn.is_autocommit() {
            return;
        }
        if let Ok(mut pool) = self.readers.lock() {
            pool.push(conn);
        }
    }

    pub(crate) fn read<T>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T, Error>,
    ) -> Result<T, Error> {
        #[cfg(test)]
        READ_TXS.with(|n| n.set(n.get() + 1));
        let wait_span = crate::stage_trace::span("coord.read.slot_wait");
        let _slot = ReaderSlot::acquire(&self.reader_count, &self.reader_idle)?;
        let _lifecycle = self
            .lifecycle
            .read()
            .map_err(|_| unavailable("lifecycle lock poisoned"))?;
        let writer = self
            .writer
            .lock()
            .map_err(|_| unavailable("writer lock poisoned"))?;
        if writer.is_none() {
            return Err(unavailable("coordinator is shut down"));
        }
        drop(writer);
        drop(wait_span);
        self.lease.validate().map_err(unavailable)?;
        let mut conn = self.take_reader()?;
        // The transaction owns the snapshot: a pooled connection sees the
        // same committed state as a freshly opened one. Identity and lease
        // checks still run on every read, so a tampered or swapped file is
        // caught exactly as before.
        let outcome = (|| {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Deferred)
                .map_err(storage)?;
            if inspect_schema_identity(&tx)? != self.expected {
                return Err(integrity("schema identity changed"));
            }
            // XP-041: evidence / source-binding rows verified once per READ
            // TRANSACTION and shared by every chain traversal inside it.
            let _digest_memo = super::compact_versions::read_cache_scope();
            let value = f(&tx)?;
            self.lease.validate().map_err(unavailable)?;
            if inspect_schema_identity(&tx)? != self.expected {
                return Err(integrity("schema identity changed"));
            }
            tx.commit().map_err(storage)?;
            Ok(value)
        })();
        self.release_reader(conn);
        outcome
    }

    fn run_checkpoint(
        &self,
        writer: &Connection,
        mode: CheckpointMode,
    ) -> Result<CheckpointMetrics, Error> {
        self.verify_boundary(writer)?;
        let management = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(storage)?;
        management.busy_timeout(self.busy).map_err(storage)?;
        verify_wal(&management)?;
        set_pragma(&management, "foreign_keys", 1)?;
        set_pragma(&management, "synchronous", 2)?;
        set_pragma(&management, "wal_autocheckpoint", 1000)?;
        set_pragma(&management, "journal_size_limit", JOURNAL_SIZE_LIMIT)?;
        self.lease.validate().map_err(unavailable)?;
        if inspect_schema_identity(&management)? != self.expected {
            return Err(integrity("schema identity changed"));
        }
        let wal_bytes_before = self.wal_bytes();
        let sql = match mode {
            CheckpointMode::Passive => "PRAGMA wal_checkpoint(PASSIVE)",
            CheckpointMode::Truncate => "PRAGMA wal_checkpoint(TRUNCATE)",
        };
        let (busy, log_frames, checkpointed_frames) = management
            .query_row(sql, [], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })
            .map_err(storage)?;
        self.lease.validate().map_err(unavailable)?;
        if inspect_schema_identity(&management)? != self.expected {
            return Err(integrity("schema identity changed"));
        }
        let wal_bytes_after = self.wal_bytes();
        Ok(CheckpointMetrics {
            mode,
            busy,
            log_frames,
            checkpointed_frames,
            wal_bytes_before,
            wal_bytes_after,
        })
    }

    pub(crate) fn checkpoint_passive(&self) -> Result<CheckpointMetrics, Error> {
        let _lifecycle = self
            .lifecycle
            .read()
            .map_err(|_| unavailable("lifecycle lock poisoned"))?;
        let writer = self
            .writer
            .lock()
            .map_err(|_| unavailable("writer lock poisoned"))?;
        let Some(conn) = writer.as_ref() else {
            return Err(unavailable("coordinator is shut down"));
        };
        self.run_checkpoint(conn, CheckpointMode::Passive)
    }

    pub(crate) fn checkpoint_idle(&self) -> Result<CheckpointMetrics, Error> {
        let _lifecycle = self
            .lifecycle
            .write()
            .map_err(|_| unavailable("lifecycle lock poisoned"))?;
        let writer = self
            .writer
            .lock()
            .map_err(|_| unavailable("writer lock poisoned"))?;
        let Some(conn) = writer.as_ref() else {
            return Err(unavailable("coordinator is shut down"));
        };
        let metrics = self.run_checkpoint(conn, CheckpointMode::Truncate)?;
        if metrics.busy != 0 || metrics.wal_bytes_after != 0 {
            return Err(unavailable("WAL did not drain"));
        }
        Ok(metrics)
    }

    pub(crate) fn shutdown(&self) -> Result<CheckpointMetrics, Error> {
        let _lifecycle = self
            .lifecycle
            .write()
            .map_err(|_| unavailable("lifecycle lock poisoned"))?;
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| unavailable("writer lock poisoned"))?;
        let Some(conn) = writer.as_ref() else {
            return Err(unavailable("coordinator is shut down"));
        };
        let metrics = self.run_checkpoint(conn, CheckpointMode::Truncate)?;
        if metrics.busy != 0 || metrics.wal_bytes_after != 0 {
            return Err(unavailable("WAL did not drain; coordinator remains usable"));
        }
        let Some(conn) = writer.take() else {
            return Err(unavailable("writer missing during shutdown"));
        };
        if let Err((conn, error)) = conn.close() {
            *writer = Some(conn);
            return Err(unavailable(format!("writer close failed: {error}")));
        }
        Ok(metrics)
    }

    fn wal_bytes(&self) -> u64 {
        let mut wal = self.path.as_os_str().to_os_string();
        wal.push("-wal");
        let wal = PathBuf::from(wal);
        std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0)
    }
}

fn recover_leak(conn: &Connection) -> Result<(), Error> {
    if conn.is_autocommit() {
        return Ok(());
    }
    conn.execute_batch("ROLLBACK")
        .map_err(|error| unavailable(format!("rollback failed: {error}")))?;
    if !conn.is_autocommit() {
        return Err(integrity("connection remains transactional after rollback"));
    }
    Ok(())
}

fn verify_wal(conn: &Connection) -> Result<(), Error> {
    let mode: String = conn
        .pragma_query_value(None, "journal_mode", |r| r.get(0))
        .map_err(storage)?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(unavailable("journal_mode is not WAL"));
    }
    Ok(())
}

fn set_pragma(conn: &Connection, name: &str, value: i64) -> Result<(), Error> {
    conn.pragma_update(None, name, value).map_err(storage)?;
    let back: i64 = conn
        .pragma_query_value(None, name, |r| r.get(0))
        .map_err(storage)?;
    if back != value {
        return Err(unavailable(format!("pragma {name} readback mismatch")));
    }
    Ok(())
}

fn open_writer(path: &Path, busy: Duration) -> Result<Connection, Error> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(storage)?;
    conn.busy_timeout(busy).map_err(storage)?;
    verify_wal(&conn)?;
    set_pragma(&conn, "foreign_keys", 1)?;
    set_pragma(&conn, "synchronous", 2)?;
    set_pragma(&conn, "wal_autocheckpoint", 1000)?;
    set_pragma(&conn, "journal_size_limit", JOURNAL_SIZE_LIMIT)?;
    conn.set_prepared_statement_cache_capacity(WRITER_CACHE);
    if !conn.is_autocommit() {
        return Err(integrity("writer connection is not in autocommit"));
    }
    Ok(conn)
}

#[cfg(test)]
mod tests;
