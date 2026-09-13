use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{FileTypeExt, MetadataExt, OpenOptionsExt},
            net::{UnixListener, UnixStream},
        },
    },
    path::{Path, PathBuf},
};
fn invalid(message: &str) -> io::Error {
    io::Error::other(message)
}
fn uid() -> u32 {
    // SAFETY: geteuid takes no arguments and does not modify memory.
    unsafe { libc::geteuid() }
}
fn private_metadata(path: &Path, directory: bool) -> io::Result<fs::Metadata> {
    let m = fs::symlink_metadata(path)?;
    if m.uid() != uid()
        || m.mode() & 0o077 != 0
        || m.file_type().is_symlink()
        || (directory && !m.is_dir())
    {
        return Err(invalid(
            "runtime path must be owned by this UID, private, and not a symlink",
        ));
    }
    Ok(m)
}
fn parent(path: &Path) -> io::Result<()> {
    private_metadata(
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| invalid("socket requires an explicit private parent directory"))?,
        true,
    )?;
    Ok(())
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    device: u64,
    inode: u64,
}
impl Identity {
    fn of(m: &fs::Metadata) -> Self {
        Self {
            device: m.dev(),
            inode: m.ino(),
        }
    }
}
/// flock is separate from SQLite's fcntl transaction locks; it identifies the
/// actual DB inode, so changing the socket name cannot create a second service.
pub struct DatabaseLease {
    file: File,
    path: PathBuf,
    identity: Identity,
}
impl DatabaseLease {
    pub fn acquire(path: &Path) -> io::Result<Self> {
        let m = private_metadata(path, false)?;
        if !m.is_file() || m.nlink() != 1 {
            return Err(invalid("database must be one regular file"));
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?;
        file.try_lock().map_err(|e| {
            invalid(&format!(
                "database service already active or lock unavailable: {e}"
            ))
        })?;
        let lease = Self {
            identity: Identity::of(&file.metadata()?),
            path: path.into(),
            file,
        };
        if lease.identity != Identity::of(&m) {
            return Err(invalid(
                "database identity changed while acquiring service lease",
            ));
        }
        lease.validate()?;
        Ok(lease)
    }
    pub fn validate(&self) -> io::Result<()> {
        let m = private_metadata(&self.path, false)?;
        if !m.is_file()
            || m.nlink() != 1
            || Identity::of(&m) != self.identity
            || Identity::of(&self.file.metadata()?) != self.identity
        {
            return Err(invalid(
                "database identity changed; stop rather than switch databases",
            ));
        }
        Ok(())
    }
}
fn peer(stream: &UnixStream) -> io::Result<()> {
    let mut credentials = std::mem::MaybeUninit::<libc::ucred>::uninit();
    let mut size = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: stream owns a live socket; both output pointers have the advertised sizes.
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            credentials.as_mut_ptr().cast(),
            &mut size,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    if size as usize != std::mem::size_of::<libc::ucred>() {
        return Err(invalid("unexpected peer credential shape"));
    }
    // SAFETY: successful getsockopt filled a complete ucred, checked above.
    if unsafe { credentials.assume_init() }.uid != uid() {
        return Err(invalid("forbidden peer UID"));
    }
    Ok(())
}
pub fn connect(path: &Path) -> io::Result<UnixStream> {
    parent(path)?;
    let m = private_metadata(path, false)?;
    if !m.file_type().is_socket() {
        return Err(invalid("endpoint is not a socket"));
    }
    let stream = UnixStream::connect(path)?;
    peer(&stream)?;
    if Identity::of(&private_metadata(path, false)?) != Identity::of(&m) {
        return Err(invalid("endpoint changed during connection"));
    }
    Ok(stream)
}
pub struct Endpoint {
    pub(crate) listener: UnixListener,
    path: PathBuf,
    identity: Identity,
}
impl Endpoint {
    pub fn bind(path: &Path, db: &DatabaseLease) -> io::Result<Self> {
        parent(path)?;
        db.validate()?;
        let owner = path.with_file_name(format!(
            "{}.owner.json",
            path.file_name()
                .ok_or_else(|| invalid("missing socket filename"))?
                .to_string_lossy()
        ));
        if fs::symlink_metadata(path).is_ok() && fs::symlink_metadata(&owner).is_err() {
            return Err(invalid("unregistered existing endpoint; no clobber"));
        }
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&owner)
        {
            Ok(mut file) => {
                // A pre-existing socket without our owner record is never adopted.
                if fs::symlink_metadata(path).is_ok() {
                    return Err(invalid("unregistered existing endpoint; no clobber"));
                }
                file.write_all(&serde_json::to_vec(&db.identity)?)?;
                file.sync_all()?;
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                let m = private_metadata(&owner, false)?;
                if !m.is_file() || m.nlink() != 1 {
                    return Err(invalid("invalid endpoint owner file"));
                }
                let mut raw = Vec::new();
                OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(&owner)?
                    .take(1025)
                    .read_to_end(&mut raw)?;
                if raw.len() > 1024 || serde_json::from_slice::<Identity>(&raw)? != db.identity {
                    return Err(invalid("endpoint belongs to another database"));
                }
            }
            Err(e) => return Err(e),
        }
        match fs::symlink_metadata(path) {
            Ok(m) => {
                private_metadata(path, false)?;
                if !m.file_type().is_socket() {
                    return Err(invalid("existing endpoint is not a socket"));
                }
                match UnixStream::connect(path) {
                    Ok(_) => return Err(invalid("endpoint already active")),
                    Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {}
                    Err(e) => return Err(e),
                }
                if Identity::of(&fs::symlink_metadata(path)?) != Identity::of(&m) {
                    return Err(invalid("endpoint changed before stale cleanup"));
                }
                fs::remove_file(path)?;
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let listener = UnixListener::bind(path)?;
        fs::set_permissions(
            path,
            <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o600),
        )?;
        listener.set_nonblocking(true)?;
        let identity = Identity::of(&private_metadata(path, false)?);
        Ok(Self {
            listener,
            path: path.into(),
            identity,
        })
    }
    pub(crate) fn authorize(&self, stream: &UnixStream) -> io::Result<()> {
        peer(stream)
    }
}
impl Drop for Endpoint {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path)
            .is_ok_and(|m| m.file_type().is_socket() && Identity::of(&m) == self.identity)
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}
