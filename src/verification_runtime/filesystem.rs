//! Inspect the actual open descriptor, not a caller declaration or mount-path guess.
use super::{Error, unverified};
use std::{fs::File, os::fd::AsRawFd};
fn supported(kind: u64) -> bool {
    // Linux UAPI include/uapi/linux/magic.h: ext2/3/4, XFS, Btrfs, tmpfs.
    matches!(kind, 0xef53 | 0x58465342 | 0x9123683e | 0x01021994)
}
pub(super) fn require_local(file: &File) -> Result<(), Error> {
    let mut info = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: File owns a live fd for this call; info points to writable statfs storage.
    if unsafe { libc::fstatfs(file.as_raw_fd(), info.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: successful fstatfs initialized the complete output structure.
    let kind = unsafe { info.assume_init() }.f_type as u64 & u64::from(u32::MAX);
    if !supported(kind) {
        return Err(unverified(
            "filesystem is outside the local runtime-guard contract",
        ));
    }
    Ok(())
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
