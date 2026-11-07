//! Process-wide resource limits shared by every connection's read path.
use super::cursor_cache::{CursorLimits, CursorPool};
use crate::market_storage::Error;
use std::{
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

/// Heap one read needs, as a multiple of the snapshot's serialized size: the
/// snapshot itself (1.45x), the manifest clone, the page slice, the columnar
/// tree and the response text. A v2 page no longer carries a copy of the
/// manifest (VM1-41; it was about 0.9x of the serialized size). Measured with
/// `tests/memory` `probe_transient_model` on fabricated fixtures, 1,000 to
/// 9,300 entities: 3.7x to 4.07x for the first page and 2.5x for a cached one
/// (before: 4.6x to 5.0x and 3.4x). The largest case is 2% above 4; the 15%
/// between the soft limit and `memory.high` covers it.
const TRANSIENT_FACTOR: usize = 4;
/// Share of `memory.high` the service lets itself use before it refuses new
/// snapshot builds. The rest is left to the daily-update worker, SQLite and
/// the page cache of the same cgroup.
const SOFT_LIMIT_PERCENT: usize = 85;
/// Headroom a fresh query reserves while its snapshot is built and measured,
/// before the size is known. A whole-market one-day read (9.4 MiB serialized)
/// peaks at 37 MiB (46 MiB before VM1-41; the build phase alone was not
/// measured, so the headroom is kept). The reservation is replaced by
/// `TRANSIENT_FACTOR` times the real size as soon as it is known.
const BUILD_RESERVE: usize = 48 * 1024 * 1024;

/// At most one `memory_budget_exceeded` line on stderr per this interval.
const REFUSAL_LOG_INTERVAL: Duration = Duration::from_secs(10);

/// Rate limiter of the refusal log line: the time of the last line and the
/// refusals dropped since.
#[derive(Default)]
struct RefusalLog {
    last: Option<Instant>,
    suppressed: u64,
}

impl RefusalLog {
    /// The line to write for a refusal at `now`, or `None` when one was
    /// written less than [`REFUSAL_LOG_INTERVAL`] ago (the refusal is counted
    /// and reported by the next line).
    fn line(&mut self, now: Instant, message: &str) -> Option<String> {
        if self
            .last
            .is_some_and(|last| now.saturating_duration_since(last) < REFUSAL_LOG_INTERVAL)
        {
            self.suppressed += 1;
            return None;
        }
        self.last = Some(now);
        let suppressed = std::mem::take(&mut self.suppressed);
        Some(format!(
            "read refused: {message} ({suppressed} more refused since the last line)"
        ))
    }
}

type Probe = Box<dyn Fn() -> Option<usize> + Send + Sync>;

/// Refuses reads while the process footprint is close to its memory limit.
///
/// Without it, a cgroup at `memory.high` does not fail the request: the
/// kernel reclaims and swaps in the allocating thread, every request slows
/// down, and the service looks hung (2026-11-05 incident). With it, the read
/// returns `memory_budget_exceeded` at once and keeps no new snapshot.
pub struct ReadBudget {
    soft_limit: Option<usize>,
    probe: Probe,
    /// Heap promised to admitted reads. Checked and raised under one lock, so
    /// reads about to be refused never count against each other.
    reserved: Mutex<usize>,
    /// What the budget does, for the startup line: its limit, or why it is inert.
    status: String,
    refusal_log: Mutex<RefusalLog>,
}

/// In-flight heap of one admitted read; released on drop.
pub struct Permit {
    budget: Arc<ReadBudget>,
    bytes: usize,
}

impl Drop for Permit {
    fn drop(&mut self) {
        *self.budget.lock() -= self.bytes;
    }
}

impl ReadBudget {
    /// Never refuses (stdio, tests).
    pub fn unlimited() -> Self {
        Self::with_probe(None, || None)
    }

    /// Footprint is the process's anonymous memory, resident plus swapped; the limit is
    /// `SOFT_LIMIT_PERCENT` of its cgroup's `memory.high`/`memory.max`, and
    /// the budget stays inert when the cgroup has no limit.
    pub fn from_cgroup() -> Self {
        let (limit, status) = match crate::memory::cgroup_limits() {
            Err(reason) => (None, format!("inactive, cgroup not readable: {reason}")),
            Ok(limits) => match limits.effective() {
                None => (
                    None,
                    "inactive, the cgroup has no memory.high or memory.max".to_owned(),
                ),
                Some(effective) if crate::memory::anon_bytes().is_none() => (
                    None,
                    format!(
                        "inactive, /proc/self/status has no RssAnon (cgroup limit {} MiB)",
                        effective >> 20
                    ),
                ),
                Some(effective) => {
                    let soft = effective / 100 * SOFT_LIMIT_PERCENT;
                    let show = |limit: Option<usize>| {
                        limit.map_or("max".to_owned(), |bytes| format!("{} MiB", bytes >> 20))
                    };
                    (
                        Some(soft),
                        format!(
                            "refuses new reads above {} MiB ({SOFT_LIMIT_PERCENT}% of the cgroup \
                             limit {} MiB: memory.high {}, memory.max {})",
                            soft >> 20,
                            effective >> 20,
                            show(limits.high),
                            show(limits.max)
                        ),
                    )
                }
            },
        };
        let mut budget = Self::with_probe(limit, crate::memory::anon_bytes);
        budget.status = status;
        budget
    }

    /// One line saying whether the budget is in force and at which limit.
    pub fn status(&self) -> &str {
        &self.status
    }

    /// Explicit limit and footprint source, for tests.
    pub fn with_probe(
        soft_limit: Option<usize>,
        probe: impl Fn() -> Option<usize> + Send + Sync + 'static,
    ) -> Self {
        Self {
            soft_limit,
            probe: Box::new(probe),
            reserved: Mutex::new(0),
            status: match soft_limit {
                Some(limit) => format!("refuses new reads above {} MiB", limit >> 20),
                None => "inactive, no limit".to_owned(),
            },
            refusal_log: Mutex::new(RefusalLog::default()),
        }
    }

    pub fn soft_limit(&self) -> Option<usize> {
        self.soft_limit
    }

    /// Heap currently reserved by admitted reads.
    pub fn reserved(&self) -> usize {
        *self.lock()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, usize> {
        self.reserved.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Before building a snapshot of unknown size. The permit is held while
    /// the snapshot is built, so concurrent builds add up too.
    pub(super) fn reserve_build(self: &Arc<Self>) -> Result<Permit, Error> {
        self.admit(BUILD_RESERVE)
    }

    /// For a derived read that names its own heap need (D-82). The caller
    /// measured the decode of the shared inputs; the number is not a snapshot
    /// size, so it is reserved as given.
    pub(super) fn reserve_derived(self: &Arc<Self>, bytes: usize) -> Result<Permit, Error> {
        self.admit(bytes)
    }

    /// For a snapshot of `snapshot_bytes` serialized bytes about to be paged.
    pub(super) fn reserve(self: &Arc<Self>, snapshot_bytes: usize) -> Result<Permit, Error> {
        self.admit(snapshot_bytes.saturating_mul(TRANSIENT_FACTOR))
    }

    fn admit(self: &Arc<Self>, need: usize) -> Result<Permit, Error> {
        let permit = || Permit {
            budget: self.clone(),
            bytes: need,
        };
        let Some(limit) = self.soft_limit else {
            *self.lock() += need;
            return Ok(permit());
        };
        let mut trimmed = false;
        loop {
            let Some(footprint) = (self.probe)() else {
                *self.lock() += need;
                return Ok(permit());
            };
            let mut reserved = self.lock();
            let before = *reserved;
            if footprint + before + need <= limit {
                *reserved += need;
                return Ok(permit());
            }
            drop(reserved);
            if !trimmed {
                // Memory freed by earlier requests may still sit in the
                // arenas; only what stays resident after returning it is
                // real load.
                trimmed = true;
                crate::memory::release_free_memory();
                continue;
            }
            let numbers = format!(
                "resident {} MiB + {} MiB in flight + {} MiB for this read > {} MiB",
                footprint >> 20,
                before >> 20,
                need >> 20,
                limit >> 20
            );
            let line = self
                .refusal_log
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .line(
                    Instant::now(),
                    &format!("memory_budget_exceeded: {numbers}"),
                );
            if let Some(line) = line {
                eprintln!("{line}");
            }
            return Err(Error::Invalid(format!(
                "memory_budget_exceeded: the data service is near its memory limit \
                 ({numbers}); \
                 this read was not served and no new snapshot was kept. Retry shortly, \
                 or narrow the selection (fewer entities, a shorter date range)"
            )));
        }
    }
}

/// The limits one process shares across all connections. Build it once and
/// hand a clone to every `DataApi`/`Queries`.
#[derive(Clone)]
pub struct ReadLimits {
    pub(super) pool: Arc<CursorPool>,
    pub(super) budget: Arc<ReadBudget>,
}

impl ReadLimits {
    /// The service configuration: [`CursorLimits::PRODUCTION`] and a budget
    /// derived from the service's own cgroup.
    pub fn production() -> Self {
        Self::new(CursorLimits::PRODUCTION, ReadBudget::from_cgroup())
    }

    pub fn new(cursors: CursorLimits, budget: ReadBudget) -> Self {
        Self {
            pool: CursorPool::new(cursors),
            budget: Arc::new(budget),
        }
    }

    pub fn pool(&self) -> &Arc<CursorPool> {
        &self.pool
    }

    pub fn budget(&self) -> &Arc<ReadBudget> {
        &self.budget
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: usize = 1024 * 1024;

    fn budget(limit: usize, resident: usize) -> Arc<ReadBudget> {
        Arc::new(ReadBudget::with_probe(Some(limit), move || Some(resident)))
    }

    #[test]
    fn reservations_of_concurrent_reads_add_up_and_are_released() {
        // 100 MiB limit, 60 MiB resident: 40 MiB of room, 4x per snapshot.
        let budget = budget(100 * MIB, 60 * MIB);
        let first = budget.reserve(4 * MIB).unwrap(); // 16 MiB
        let second = budget.reserve(4 * MIB).unwrap(); // 16 MiB, 32 in flight
        assert_eq!(budget.reserved(), 32 * MIB);
        // A third 20 MiB read would put 60 + 32 + 20 over 100.
        let refused = budget
            .reserve(5 * MIB)
            .err()
            .expect("over the limit")
            .to_string();
        assert!(
            refused.starts_with("INVALID_MARKET_COMMAND: memory_budget_exceeded"),
            "{refused}"
        );
        // A refused read leaves no reservation behind.
        assert_eq!(budget.reserved(), 32 * MIB);
        drop(first);
        assert_eq!(budget.reserved(), 16 * MIB);
        let third = budget.reserve(4 * MIB).unwrap();
        drop((second, third));
        assert_eq!(budget.reserved(), 0);
    }

    #[test]
    fn reservations_race_without_overshooting_the_limit() {
        let budget = budget(100 * MIB, 60 * MIB);
        let admitted = std::sync::atomic::AtomicUsize::new(0);
        use std::sync::atomic::Ordering;
        let start = std::sync::Barrier::new(16);
        std::thread::scope(|scope| {
            for _ in 0..16 {
                scope.spawn(|| {
                    start.wait();
                    if let Ok(permit) = budget.reserve(4 * MIB) {
                        admitted.fetch_add(1, Ordering::SeqCst);
                        // Hold until every thread has tried.
                        std::thread::sleep(std::time::Duration::from_millis(200));
                        drop(permit);
                    }
                });
            }
        });
        // 40 MiB of room / 16 MiB each: two fit, however the threads interleave.
        assert_eq!(admitted.load(Ordering::SeqCst), 2);
        assert_eq!(budget.reserved(), 0);
    }

    #[test]
    fn a_snapshot_build_holds_its_headroom_until_dropped() {
        let budget = budget(120 * MIB, 60 * MIB);
        let building = budget.reserve_build().unwrap();
        assert_eq!(budget.reserved(), BUILD_RESERVE);
        // A second build would need 60 + 48 + 48 > 120.
        assert!(budget.reserve_build().is_err());
        drop(building);
        assert_eq!(budget.reserved(), 0);
        let tight = self::budget(120 * MIB, 80 * MIB);
        assert!(tight.reserve_build().is_err());
        assert_eq!(tight.reserved(), 0);
    }

    #[test]
    fn the_refusal_log_writes_one_line_per_interval_and_counts_the_rest() {
        let t0 = Instant::now();
        let mut log = RefusalLog::default();
        let first = log.line(t0, "memory_budget_exceeded: a").unwrap();
        assert_eq!(
            first,
            "read refused: memory_budget_exceeded: a (0 more refused since the last line)"
        );
        assert!(log.line(t0 + Duration::from_secs(1), "b").is_none());
        assert!(log.line(t0 + Duration::from_secs(9), "c").is_none());
        let next = log.line(t0 + Duration::from_secs(10), "d").unwrap();
        assert!(
            next.ends_with("d (2 more refused since the last line)"),
            "{next}"
        );
        // The count starts again after a line.
        assert!(log.line(t0 + Duration::from_secs(11), "e").is_none());
        let last = log.line(t0 + Duration::from_secs(25), "f").unwrap();
        assert!(
            last.ends_with("f (1 more refused since the last line)"),
            "{last}"
        );
    }

    #[test]
    fn status_names_the_limit_or_says_inert() {
        assert_eq!(
            budget(100 * MIB, 0).status(),
            "refuses new reads above 100 MiB"
        );
        assert!(ReadBudget::unlimited().status().starts_with("inactive"));
    }

    #[test]
    fn unreadable_footprint_never_refuses() {
        let budget = Arc::new(ReadBudget::with_probe(Some(MIB), || None));
        budget.reserve(64 * MIB).unwrap();
    }
}
