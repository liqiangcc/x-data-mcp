//! Pagination cursors of the read path, held in one process-wide pool.
//!
//! Before VM1-23 every `Queries` (one per connection thread) owned a private
//! 64 MiB cache, so the process held up to `max_clients` times that, and an
//! idle connection pinned its share for as long as it stayed open. Now every
//! connection is an *owner* in a shared [`CursorPool`]:
//!
//! * `total_bytes` bounds the sum of distinct snapshots of all owners; the
//!   globally least recently used entry goes first.
//! * `owner_bytes` / `owner_entries` bound one owner, so a single connection
//!   cannot evict everybody else's cursors.
//! * Dropping an owner's [`CursorCache`] (connection close) frees its entries.
//! * A cursor resumes only for the owner that received it.
//!
//! Bytes are the serialized size recorded at the snapshot-budget check. The
//! heap behind a snapshot is about 1.45x that (measured, `tests/memory`).
use super::{Access, query::QuerySnapshot, wire::VersionInfo};
use chrono::{DateTime, Utc};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
};

#[derive(Clone)]
pub(super) struct Saved {
    pub(super) token: String,
    pub(super) key: String,
    pub(super) access: Access,
    pub(super) time: DateTime<Utc>,
    pub(super) id: String,
    pub(super) offset: usize,
    pub(super) content: Content,
    /// Serialized size recorded at the snapshot-budget check, so resumed
    /// entries never re-serialize. Entries sharing `id` point at the same
    /// `Arc`'d snapshot and are accounted once.
    pub(super) bytes: usize,
}

#[derive(Clone)]
pub(super) enum Content {
    Data(Arc<QuerySnapshot>),
    Versions(Arc<Vec<VersionInfo>>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorLimits {
    /// Distinct-snapshot bytes over all owners.
    pub total_bytes: usize,
    /// Distinct-snapshot bytes of one owner.
    pub owner_bytes: usize,
    /// Cursor entries of one owner.
    pub owner_entries: usize,
}

impl CursorLimits {
    /// Service limits: 48 MiB of serialized snapshots in the whole process
    /// (about 70 MiB of heap), at most half of it for one connection. A
    /// whole-market `universe` snapshot is 9.4 MiB serialized, the budget
    /// check allows 16 MiB.
    pub const PRODUCTION: Self = Self {
        total_bytes: 48 * 1024 * 1024,
        owner_bytes: 24 * 1024 * 1024,
        owner_entries: 32,
    };
}

struct Entry {
    owner: u64,
    saved: Saved,
}

#[derive(Default)]
struct Inner {
    /// Least recently used first.
    entries: VecDeque<Entry>,
    /// Live entry count per snapshot id (page cursors of one query share
    /// `id`, and the snapshot, via resume/save).
    refs: HashMap<String, u32>,
    bytes: usize,
    owner_bytes: HashMap<u64, usize>,
}

impl Inner {
    fn release(&mut self, entry: &Entry) {
        let Some(n) = self.refs.get_mut(&entry.saved.id) else {
            return;
        };
        *n -= 1;
        if *n == 0 {
            self.refs.remove(&entry.saved.id);
            self.bytes -= entry.saved.bytes;
            if let Some(owned) = self.owner_bytes.get_mut(&entry.owner) {
                *owned -= entry.saved.bytes;
                if *owned == 0 {
                    self.owner_bytes.remove(&entry.owner);
                }
            }
        }
    }

    /// Evict the least recently used entry, of `owner` if given.
    fn evict(&mut self, owner: Option<u64>) -> bool {
        let Some(at) = self
            .entries
            .iter()
            .position(|e| owner.is_none_or(|o| e.owner == o))
        else {
            return false;
        };
        let entry = self.entries.remove(at).expect("position is in range");
        self.release(&entry);
        true
    }
}

pub struct CursorPool {
    limits: CursorLimits,
    next_owner: AtomicU64,
    inner: Mutex<Inner>,
}

impl CursorPool {
    pub fn new(limits: CursorLimits) -> Arc<Self> {
        Arc::new(Self {
            limits,
            next_owner: AtomicU64::new(0),
            inner: Mutex::new(Inner::default()),
        })
    }

    pub fn limits(&self) -> CursorLimits {
        self.limits
    }

    /// (entries, distinct-snapshot bytes) over all owners.
    pub fn totals(&self) -> (usize, usize) {
        let inner = self.lock();
        (inner.entries.len(), inner.bytes)
    }

    /// Distinct-snapshot bytes of one owner, for tests.
    #[doc(hidden)]
    pub fn owner_bytes(&self, owner: u64) -> usize {
        self.lock().owner_bytes.get(&owner).copied().unwrap_or(0)
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // The state is plain data; a panic elsewhere must not wedge every
        // other connection's pagination.
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn push(&self, owner: u64, saved: Saved) {
        let limits = self.limits;
        let mut inner = self.lock();
        // Evicting can drop the last entry of `saved.id`, which changes
        // whether the snapshot is already accounted: recompute every round.
        let new_bytes = |inner: &Inner| {
            if inner.refs.contains_key(&saved.id) {
                0
            } else {
                saved.bytes
            }
        };
        loop {
            let entries = inner.entries.iter().filter(|e| e.owner == owner).count();
            let owned = inner.owner_bytes.get(&owner).copied().unwrap_or(0);
            if entries < limits.owner_entries && owned + new_bytes(&inner) <= limits.owner_bytes {
                break;
            }
            if !inner.evict(Some(owner)) {
                break;
            }
        }
        while inner.bytes + new_bytes(&inner) > limits.total_bytes {
            if !inner.evict(None) {
                break;
            }
        }
        let added = new_bytes(&inner);
        *inner.refs.entry(saved.id.clone()).or_insert(0) += 1;
        inner.bytes += added;
        if added > 0 {
            *inner.owner_bytes.entry(owner).or_insert(0) += added;
        }
        inner.entries.push_back(Entry { owner, saved });
    }

    /// LRU: resuming a cursor marks it most recently used.
    fn resume(&self, owner: u64, token: &str) -> Option<Saved> {
        let mut inner = self.lock();
        let at = inner
            .entries
            .iter()
            .position(|e| e.owner == owner && e.saved.token == token)?;
        let entry = inner.entries.remove(at)?;
        let saved = entry.saved.clone();
        inner.entries.push_back(entry);
        Some(saved)
    }

    /// Drop every entry of `owner`; returns the distinct bytes it held.
    fn purge(&self, owner: u64) -> usize {
        let mut inner = self.lock();
        let held = inner.owner_bytes.get(&owner).copied().unwrap_or(0);
        let mut kept = VecDeque::with_capacity(inner.entries.len());
        let mut dropped = Vec::new();
        for entry in std::mem::take(&mut inner.entries) {
            if entry.owner == owner {
                dropped.push(entry);
            } else {
                kept.push_back(entry);
            }
        }
        inner.entries = kept;
        for entry in &dropped {
            inner.release(entry);
        }
        drop(inner);
        // Snapshots are freed here, outside the lock.
        drop(dropped);
        held
    }
}

/// One connection's view of the pool. Dropping it frees the connection's
/// cursors and hands the freed arena pages back to the kernel.
pub(super) struct CursorCache {
    pool: Arc<CursorPool>,
    owner: u64,
}

impl CursorCache {
    pub(super) fn new(pool: Arc<CursorPool>) -> Self {
        let owner = pool.next_owner.fetch_add(1, Ordering::Relaxed) + 1;
        Self { pool, owner }
    }

    pub(super) fn owner(&self) -> u64 {
        self.owner
    }

    pub(super) fn pool(&self) -> &CursorPool {
        &self.pool
    }

    pub(super) fn push(&self, saved: Saved) {
        self.pool.push(self.owner, saved);
    }

    pub(super) fn resume(&self, token: &str) -> Option<Saved> {
        self.pool.resume(self.owner, token)
    }

    /// (entries, distinct-snapshot bytes, distinct snapshots) of this owner.
    pub(super) fn stats(&self) -> (usize, usize, usize) {
        let inner = self.pool.lock();
        let mut ids = std::collections::HashSet::new();
        let entries = inner
            .entries
            .iter()
            .filter(|e| e.owner == self.owner)
            .inspect(|e| {
                ids.insert(e.saved.id.as_str());
            })
            .count();
        (
            entries,
            inner.owner_bytes.get(&self.owner).copied().unwrap_or(0),
            ids.len(),
        )
    }
}

impl Drop for CursorCache {
    fn drop(&mut self) {
        if self.pool.purge(self.owner) >= crate::memory::TRIM_AFTER_RESPONSE_BYTES {
            crate::memory::release_free_memory();
        }
    }
}
