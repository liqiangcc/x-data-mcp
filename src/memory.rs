//! Process memory helpers for the long-lived data service (VM1-23).
//!
//! Two facts drive this module, both measured with `tests/memory/probe.rs`:
//! glibc keeps memory freed by a finished request in the per-thread arena of
//! the connection thread (RSS stays high with zero live heap), and the
//! service runs inside a cgroup whose `memory.high` makes the kernel reclaim
//! (and swap) instead of failing. This module returns free arena memory to
//! the kernel and reads the process footprint and its cgroup limit so the
//! read path can refuse work before the cgroup starts to thrash.

/// A response at least this large is followed by [`release_free_memory`]:
/// a page of the whole-market `universe` read is about 2.9 MiB on the wire,
/// a single-stock page a few tens of KiB.
pub const TRIM_AFTER_RESPONSE_BYTES: usize = 1 << 20;

/// Return freed heap pages of every glibc arena to the kernel.
///
/// `malloc_trim` is glibc-only; on other targets this is a no-op.
pub fn release_free_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: `malloc_trim` takes no pointers and is thread-safe.
    unsafe {
        libc::malloc_trim(0);
    }
}

/// Anonymous memory of this process in bytes: resident (`RssAnon`) plus
/// swapped out (`VmSwap`). A process that the kernel is already swapping out
/// to stay under `memory.high` must not look small to the read budget.
pub fn anon_bytes() -> Option<usize> {
    parse_anon(&std::fs::read_to_string("/proc/self/status").ok()?)
}

/// The tighter of `memory.high` and `memory.max` of this process's cgroup,
/// `None` when it is unlimited or cannot be read (not cgroup v2, no access).
pub fn cgroup_limit() -> Option<usize> {
    let membership = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    let dir = cgroup_dir(&membership)?;
    ["memory.high", "memory.max"]
        .into_iter()
        .filter_map(|file| parse_limit(&std::fs::read_to_string(dir.join(file)).ok()?))
        .min()
}

fn parse_anon(status: &str) -> Option<usize> {
    let kib = |key: &str| -> Option<usize> {
        let line = status.lines().find(|line| line.starts_with(key))?;
        line.split_whitespace().nth(1)?.parse().ok()
    };
    // Kernels built without swap omit `VmSwap`.
    kib("RssAnon:")?
        .checked_add(kib("VmSwap:").unwrap_or(0))?
        .checked_mul(1024)
}

fn cgroup_dir(membership: &str) -> Option<std::path::PathBuf> {
    let path = membership
        .lines()
        .find_map(|line| line.strip_prefix("0::"))?
        .trim();
    // A path with a component other than plain names would escape the mount.
    if path.split('/').any(|part| part == "..") {
        return None;
    }
    Some(std::path::Path::new("/sys/fs/cgroup").join(path.trim_start_matches('/')))
}

fn parse_limit(text: &str) -> Option<usize> {
    text.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anon_is_resident_plus_swapped_in_bytes() {
        let status = "Name:\tx\nRssAnon:\t   2048 kB\nRssFile:\t 10 kB\nVmSwap:\t 1024 kB\n";
        assert_eq!(parse_anon(status), Some(3 * 1024 * 1024));
        let no_swap_line = "Name:\tx\nRssAnon:\t   2048 kB\n";
        assert_eq!(parse_anon(no_swap_line), Some(2 * 1024 * 1024));
        assert_eq!(parse_anon("Name:\tx\n"), None);
    }

    #[test]
    fn cgroup_v2_membership_maps_below_the_mount() {
        let dir = cgroup_dir("0::/system.slice/x-data-mcp-data.service\n").unwrap();
        assert_eq!(
            dir,
            std::path::Path::new("/sys/fs/cgroup/system.slice/x-data-mcp-data.service")
        );
        assert!(cgroup_dir("0::/a/../../etc\n").is_none());
        assert!(cgroup_dir("12:memory:/legacy\n").is_none());
    }

    #[test]
    fn unlimited_cgroup_value_is_no_limit() {
        assert_eq!(parse_limit("max\n"), None);
        assert_eq!(parse_limit("402653184\n"), Some(402_653_184));
    }
}
