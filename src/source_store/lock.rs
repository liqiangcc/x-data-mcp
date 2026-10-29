//! Inter-process locking for one source store (§9.4.1).
//!
//! Every writer takes an exclusive `flock` on `<store>/.lock` for the whole
//! `register` (dedupe, blob publish, manifest append, fsync); readers take a
//! shared lock while they read the manifest. `flock` is per open file
//! description, so two handles inside one process exclude each other exactly
//! like two processes do — which is what the concurrency test needs.
//!
//! The lock is advisory and only covers this store root. It is held until the
//! handle drops; there is no explicit unlock and no lock file content.
//!
//! The crate depends on `libc` on Linux only (the deployment target). Other
//! platforms fail closed instead of writing without exclusion.

#[cfg(target_os = "linux")]
mod imp {
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::path::Path;

    /// Open file description holding the lock; dropping it unlocks.
    pub(crate) struct StoreLock {
        _file: File,
    }

    fn open(root: &Path, op: libc::c_int) -> io::Result<StoreLock> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join(super::super::LOCK_FILE))?;
        // SAFETY: `AsRawFd::as_raw_fd` returns a live descriptor owned by
        // `file`, and `flock` only reads that descriptor.
        let raw = std::os::unix::io::AsRawFd::as_raw_fd(&file);
        let rc = unsafe { libc::flock(raw, op) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(StoreLock { _file: file })
    }

    pub(crate) fn exclusive(root: &Path) -> io::Result<StoreLock> {
        open(root, libc::LOCK_EX)
    }

    pub(crate) fn shared(root: &Path) -> io::Result<StoreLock> {
        open(root, libc::LOCK_SH)
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use std::io;
    use std::path::Path;

    /// Placeholder handle: the platform never reaches this module on the
    /// supported deployment target.
    pub(crate) struct StoreLock;

    pub(crate) fn exclusive(_root: &Path) -> io::Result<StoreLock> {
        Err(io::Error::other(
            "source store locking is only implemented on linux",
        ))
    }

    pub(crate) fn shared(_root: &Path) -> io::Result<StoreLock> {
        Err(io::Error::other(
            "source store locking is only implemented on linux",
        ))
    }
}

pub(crate) use imp::{StoreLock, exclusive, shared};
