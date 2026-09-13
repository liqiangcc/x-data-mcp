//! Descriptor-based local filesystem capability used by runtime locks.
use std::{fs::File, io, os::fd::AsRawFd};
fn supported(kind: u64) -> bool {
    // Linux UAPI include/uapi/linux/magic.h: ext2/3/4, XFS, Btrfs, tmpfs.
    matches!(kind, 0xef53 | 0x58465342 | 0x9123683e | 0x01021994)
}
pub(crate) fn supported_file(file: &File) -> io::Result<bool> {
    let mut info = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: live owned fd and writable statfs storage of the correct size.
    if unsafe { libc::fstatfs(file.as_raw_fd(), info.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful fstatfs initialized the output structure.
    let kind = unsafe { info.assume_init() }.f_type as u64 & u64::from(u32::MAX);
    Ok(supported(kind))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remote_overlay_and_unknown_filesystems_are_not_assumed_local() {
        for kind in [0x6969, 0xff534d42, 0x794c7630, 0x65735546, 0] {
            assert!(!supported(kind));
        }
        for kind in [0xef53, 0x58465342, 0x9123683e, 0x01021994] {
            assert!(supported(kind));
        }
    }
}
