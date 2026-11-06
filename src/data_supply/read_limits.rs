//! Process-wide resource limits shared by every connection's read path.
use super::cursor_cache::{CursorLimits, CursorPool};
use crate::market_storage::Error;
use std::sync::{Arc, Mutex, PoisonError};

/// Heap one read needs, as a multiple of the snapshot's serialized size: the
/// snapshot itself (1.45x), the manifest clone, the page slice, the columnar
/// tree and the response text (3.4x more). Measured 4.9x to 5.0x for the
/// first page and 3.4x for a cached one (`tests/memory`, `probe_transient_model`).
const TRANSIENT_FACTOR: usize = 5;
/// Share of `memory.high` the service lets itself use before it refuses new
/// snapshot builds. The rest is left to the daily-update worker, SQLite and
/// the page cache of the same cgroup.
const SOFT_LIMIT_PERCENT: usize = 85;
/// Headroom a fresh query reserves while its snapshot is built and measured,
/// before the size is known. A whole-market one-day read (9.4 MiB serialized)
/// peaks at 46 MiB; the reservation is replaced by `TRANSIENT_FACTOR` times
/// the real size as soon as it is known.
const BUILD_RESERVE: usize = 48 * 1024 * 1024;

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
        Self::with_probe(
            crate::memory::cgroup_limit().map(|limit| limit / 100 * SOFT_LIMIT_PERCENT),
            crate::memory::anon_bytes,
        )
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
            return Err(Error::Invalid(format!(
                "memory_budget_exceeded: the data service is near its memory limit \
                 (resident {} MiB + {} MiB in flight + {} MiB for this read > {} MiB); \
                 this read was not served and no new snapshot was kept. Retry shortly, \
                 or narrow the selection (fewer entities, a shorter date range)",
                footprint >> 20,
                before >> 20,
                need >> 20,
                limit >> 20
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
        // 100 MiB limit, 60 MiB resident: 40 MiB of room, 5x per snapshot.
        let budget = budget(100 * MIB, 60 * MIB);
        let first = budget.reserve(3 * MIB).unwrap(); // 15 MiB
        let second = budget.reserve(3 * MIB).unwrap(); // 15 MiB, 30 in flight
        assert_eq!(budget.reserved(), 30 * MIB);
        // A third 15 MiB read would put 60 + 30 + 15 over 100.
        let refused = budget
            .reserve(4 * MIB)
            .err()
            .expect("over the limit")
            .to_string();
        assert!(
            refused.starts_with("INVALID_MARKET_COMMAND: memory_budget_exceeded"),
            "{refused}"
        );
        // A refused read leaves no reservation behind.
        assert_eq!(budget.reserved(), 30 * MIB);
        drop(first);
        assert_eq!(budget.reserved(), 15 * MIB);
        let third = budget.reserve(3 * MIB).unwrap();
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
                    if let Ok(permit) = budget.reserve(3 * MIB) {
                        admitted.fetch_add(1, Ordering::SeqCst);
                        // Hold until every thread has tried.
                        std::thread::sleep(std::time::Duration::from_millis(200));
                        drop(permit);
                    }
                });
            }
        });
        // 40 MiB of room / 15 MiB each: two fit, however the threads interleave.
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
    fn unreadable_footprint_never_refuses() {
        let budget = Arc::new(ReadBudget::with_probe(Some(MIB), || None));
        budget.reserve(64 * MIB).unwrap();
    }
}
