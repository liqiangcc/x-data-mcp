//! Linux runtime exclusion for cooperative verification executors.
//!
//! Holding this file lock is only one part of a stopping proof. A task must
//! also have an atomically registered claim and verified durable outcome.
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, Metadata, OpenOptions, TryLockError},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, FileExt, MetadataExt},
    path::{Path, PathBuf},
};
pub const PROTOCOL: &str = "verification-file-guard-v1";
const MAX_DESCRIPTOR_BYTES: u64 = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileIdentity {
    pub device: u64,
    pub inode: u64,
}
impl FileIdentity {
    fn from_metadata(meta: Metadata) -> Result<Self, Error> {
        if !meta.is_file() || meta.nlink() != 1 {
            return Err(unverified("requires a regular, non-aliased local file"));
        }
        Ok(Self {
            device: meta.dev(),
            inode: meta.ino(),
        })
    }
    fn path(path: &Path) -> Result<Self, Error> {
        Self::from_metadata(std::fs::symlink_metadata(path)?)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuardDescriptor {
    pub protocol: String,
    pub task_id: String,
    pub run_id: String,
    pub boot_id: String,
    pub database_path: PathBuf,
    pub database_identity: FileIdentity,
    pub lock_path: PathBuf,
    pub lock_identity: FileIdentity,
}
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("verification execution guard is held")]
    Busy,
    #[error("runtime stopping evidence not verified: {0}")]
    NotVerified(String),
    #[error("runtime guard I/O: {0}")]
    Io(#[from] std::io::Error),
}
fn unverified(reason: impl Into<String>) -> Error {
    Error::NotVerified(reason.into())
}
fn boot_id() -> Result<String, Error> {
    let mut value = String::new();
    File::open("/proc/sys/kernel/random/boot_id")?
        .take(65)
        .read_to_string(&mut value)?;
    let value = value.trim();
    if value.len() != 36 || !value.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
        return Err(unverified("kernel boot identity unavailable"));
    }
    Ok(value.into())
}
struct Context {
    database: PathBuf,
    identity: FileIdentity,
    boot: String,
    directory: PathBuf,
    lock: PathBuf,
}
impl Context {
    fn new(database: &Path, task: &str, run: &str) -> Result<Self, Error> {
        if task.trim().is_empty() || run.trim().is_empty() {
            return Err(unverified("empty task/run identity"));
        }
        let database = database.canonicalize()?;
        let identity = FileIdentity::path(&database)?;
        let mut name = database.as_os_str().to_os_string();
        name.push(".verification-guards");
        let directory = PathBuf::from(name);
        let lock = directory.join(format!("{}.lock", crate::hash::sha256(task.as_bytes())));
        Ok(Self {
            database,
            identity,
            boot: boot_id()?,
            directory,
            lock,
        })
    }
    fn check_directory(&self) -> Result<(), Error> {
        if !std::fs::symlink_metadata(&self.directory)?.is_dir() {
            return Err(unverified("runtime directory is not a direct directory"));
        }
        Ok(())
    }
    fn matches(&self, descriptor: &GuardDescriptor) -> bool {
        descriptor.protocol == PROTOCOL
            && descriptor.boot_id == self.boot
            && descriptor.database_path == self.database
            && descriptor.database_identity == self.identity
            && descriptor.lock_path == self.lock
    }
}
fn lock(file: &File) -> Result<(), Error> {
    match file.try_lock() {
        Ok(()) => Ok(()),
        Err(TryLockError::WouldBlock) => Err(Error::Busy),
        Err(TryLockError::Error(error)) => Err(Error::Io(error)),
    }
}
fn read_descriptor(file: &File) -> Result<GuardDescriptor, Error> {
    let length = file.metadata()?.len();
    if length == 0 || length > MAX_DESCRIPTOR_BYTES {
        return Err(unverified(
            "runtime descriptor byte budget or missing descriptor",
        ));
    }
    let mut bytes = vec![0; length as usize];
    file.read_exact_at(&mut bytes, 0)?;
    serde_json::from_slice(&bytes)
        .map_err(|e| unverified(format!("invalid runtime descriptor: {e}")))
}

/// Not Clone; no exposed file handle or early unlock. Drop ends the protected scope.
#[derive(Debug)]
pub struct ExecutionGuard {
    file: File,
    descriptor: GuardDescriptor,
}
impl ExecutionGuard {
    pub fn descriptor(&self) -> &GuardDescriptor {
        &self.descriptor
    }

    /// Recheck all bindings while still holding the same, non-cloned handle.
    pub fn verify_binding(&self, database: &Path) -> Result<(), Error> {
        let context = Context::new(database, &self.descriptor.task_id, &self.descriptor.run_id)?;
        context.check_directory()?;
        if !context.matches(&self.descriptor)
            || FileIdentity::from_metadata(self.file.metadata()?)? != self.descriptor.lock_identity
            || FileIdentity::path(&context.lock)? != self.descriptor.lock_identity
            || read_descriptor(&self.file)? != self.descriptor
        {
            return Err(unverified("runtime/database/handle binding changed"));
        }
        Ok(())
    }
}

/// Executor-only creation. This does not register or claim a task by itself.
pub fn create(database: &Path, task: &str, run: &str) -> Result<ExecutionGuard, Error> {
    let context = Context::new(database, task, run)?;
    match std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&context.directory)
    {
        Ok(()) => File::open(context.database.parent().unwrap())?.sync_all()?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    context.check_directory()?;
    if context.lock.try_exists()? {
        return reopen_for_executor(database, task, run, &context);
    }
    // Publish an already locked and synced descriptor, never a visible empty file.
    let mut temporary = tempfile::NamedTempFile::new_in(&context.directory)?;
    lock(temporary.as_file())?;
    let descriptor = GuardDescriptor {
        protocol: PROTOCOL.into(),
        task_id: task.into(),
        run_id: run.into(),
        boot_id: context.boot,
        database_path: context.database,
        database_identity: context.identity,
        lock_path: context.lock.clone(),
        lock_identity: FileIdentity::from_metadata(temporary.as_file().metadata()?)?,
    };
    let bytes = serde_json::to_vec(&descriptor).map_err(|e| unverified(e.to_string()))?;
    if bytes.len() as u64 > MAX_DESCRIPTOR_BYTES {
        return Err(unverified("runtime descriptor exceeds byte budget"));
    }
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    let file = match temporary.persist_noclobber(&context.lock) {
        Ok(file) => file,
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            return reopen_for_executor(database, task, run, &Context::new(database, task, run)?);
        }
        Err(error) => return Err(Error::Io(error.error)),
    };
    File::open(context.directory)?.sync_all()?;
    let guard = ExecutionGuard { file, descriptor };
    guard.verify_binding(database)?;
    Ok(guard)
}
fn reopen_for_executor(
    database: &Path,
    task: &str,
    run: &str,
    context: &Context,
) -> Result<ExecutionGuard, Error> {
    // A complete orphan descriptor can be reused; a partial/corrupt one is never repaired.
    FileIdentity::path(&context.lock)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&context.lock)?;
    lock(&file)?;
    let descriptor = read_descriptor(&file)?;
    if descriptor.task_id != task || descriptor.run_id != run {
        return Err(unverified("runtime descriptor belongs to another task/run"));
    }
    let guard = ExecutionGuard { file, descriptor };
    guard.verify_binding(database)?;
    Ok(guard)
}

/// Inspector-only nonblocking acquisition. Never creates files or trusts a path alone.
pub fn try_acquire(database: &Path, expected: &GuardDescriptor) -> Result<ExecutionGuard, Error> {
    let context = Context::new(database, &expected.task_id, &expected.run_id)?;
    if !context.matches(expected) {
        return Err(unverified("registered runtime context differs"));
    }
    context.check_directory()?;
    if FileIdentity::path(&context.lock)? != expected.lock_identity {
        return Err(unverified("registered lock file was replaced"));
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&context.lock)?;
    lock(&file)?;
    let guard = ExecutionGuard {
        file,
        descriptor: expected.clone(),
    };
    guard.verify_binding(database)?;
    Ok(guard)
}

#[cfg(test)]
mod tests;
