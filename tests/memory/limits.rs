//! Regression tests for the data service's read-path memory limits (VM1-23).
//! Fabricated fixtures only: a synthetic universe, no database, no real data.
use super::fake::*;
use std::{
    sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};
use x_data_mcp::{
    data_supply::{
        cursor_cache::CursorLimits,
        query::Queries,
        read_limits::{ReadBudget, ReadLimits},
    },
    mcp::handle_json,
};

const MIB: usize = 1024 * 1024;
/// 1,000 entities: a 1.8 MiB serialized snapshot, a 0.5 MiB page.
const ENTITIES: usize = 1000;
const PAGE: usize = 100;

fn limits(total: usize, owner: usize, entries: usize) -> ReadLimits {
    ReadLimits::new(
        CursorLimits {
            total_bytes: total,
            owner_bytes: owner,
            owner_entries: entries,
        },
        ReadBudget::unlimited(),
    )
}

fn connection(limits: &ReadLimits) -> Handler {
    Handler(Queries::with_limits(Fake { entities: ENTITIES }, limits))
}

/// Run a whole-universe read to its first cursor; returns that cursor.
fn first_page(h: &Handler) -> String {
    let out = handle_json(h, &request(None, PAGE, 1)).unwrap();
    assert!(!is_error(&out), "{out}");
    next_cursor(&out).expect("1,000 entities at page 100 need more pages")
}

fn snapshot_bytes() -> usize {
    let probe = connection(&limits(64 * MIB, 64 * MIB, 32));
    first_page(&probe);
    probe.0.cache_stats().1
}

#[test]
fn fixture_snapshot_is_large_enough_to_matter() {
    let bytes = snapshot_bytes();
    assert!(bytes > MIB && bytes < 4 * MIB, "{bytes}");
}

#[test]
fn many_connections_never_exceed_the_process_wide_cap() {
    let one = snapshot_bytes();
    // Room for three snapshots in the whole process; eight connections.
    let total = one * 3 + one / 2;
    let limits = limits(total, total, 32);
    let barrier = Barrier::new(8);
    let worst = AtomicUsize::new(0);
    thread::scope(|s| {
        for _ in 0..8 {
            s.spawn(|| {
                let h = connection(&limits);
                barrier.wait();
                for round in 0..5 {
                    // Distinct id per round: every first page is a new snapshot.
                    let mut a =
                        serde_json::from_str::<serde_json::Value>(&request(None, PAGE, round))
                            .unwrap();
                    a["params"]["arguments"]["filter"]["end_date"] =
                        format!("2026-11-0{}", 5 + round % 3).into();
                    let out = handle_json(&h, &a.to_string()).unwrap();
                    assert!(!is_error(&out), "{out}");
                    worst.fetch_max(limits.pool().totals().1, Ordering::SeqCst);
                    assert!(limits.pool().totals().1 <= total);
                }
            });
        }
    });
    assert!(worst.load(Ordering::SeqCst) > 0, "the cache was never used");
    assert!(worst.load(Ordering::SeqCst) <= total);
    // Every connection is closed: nothing is left.
    assert_eq!(limits.pool().totals(), (0, 0));
}

#[test]
fn one_connection_cannot_take_the_whole_pool() {
    let one = snapshot_bytes();
    // Pool fits four snapshots, one owner may hold two.
    let limits = limits(one * 4 + one / 2, one * 2 + one / 2, 32);
    let greedy = connection(&limits);
    let quiet = connection(&limits);
    let quiet_cursor = first_page(&quiet);
    for day in 1..=6 {
        let mut a = serde_json::from_str::<serde_json::Value>(&request(None, PAGE, 1)).unwrap();
        a["params"]["arguments"]["filter"]["end_date"] = format!("2026-11-{:02}", 5 + day).into();
        let out = handle_json(&greedy, &a.to_string()).unwrap();
        assert!(!is_error(&out), "{out}");
    }
    let (_, greedy_bytes, greedy_snapshots) = greedy.0.cache_stats();
    assert!(greedy_bytes <= one * 2 + one / 2, "{greedy_bytes}");
    assert_eq!(greedy_snapshots, 2);
    // The quiet connection's cursor survived the greedy one.
    let out = handle_json(&quiet, &request(Some(&quiet_cursor), PAGE, 2)).unwrap();
    assert!(!is_error(&out), "{out}");
    assert!(limits.pool().totals().1 <= one * 4 + one / 2);
}

#[test]
fn closing_a_connection_releases_its_cursors() {
    let limits = limits(64 * MIB, 32 * MIB, 32);
    let a = connection(&limits);
    let b = connection(&limits);
    first_page(&a);
    first_page(&b);
    let (entries, bytes) = limits.pool().totals();
    assert_eq!(entries, 2);
    assert!(bytes > 0);
    let per_connection = a.0.cache_stats().1;
    drop(a);
    assert_eq!(limits.pool().totals(), (1, bytes - per_connection));
    drop(b);
    assert_eq!(limits.pool().totals(), (0, 0));
}

#[test]
fn a_cursor_only_resumes_on_the_connection_that_received_it() {
    let limits = limits(64 * MIB, 32 * MIB, 32);
    let a = connection(&limits);
    let b = connection(&limits);
    let cursor = first_page(&a);
    let stolen = handle_json(&b, &request(Some(&cursor), PAGE, 3)).unwrap();
    assert!(is_error(&stolen), "{stolen}");
    let own = handle_json(&a, &request(Some(&cursor), PAGE, 4)).unwrap();
    assert!(!is_error(&own), "{own}");
}

#[test]
fn paging_to_the_end_keeps_the_cache_within_the_cap() {
    let one = snapshot_bytes();
    let limits = limits(one * 2, one * 2, 32);
    let h = connection(&limits);
    let mut cursor = None;
    let mut pages = 0;
    loop {
        let out = handle_json(&h, &request(cursor.as_deref(), PAGE, 1)).unwrap();
        assert!(!is_error(&out), "{out}");
        assert!(limits.pool().totals().1 <= one * 2);
        pages += 1;
        cursor = next_cursor(&out);
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(pages, ENTITIES / PAGE);
}

#[test]
fn near_the_memory_limit_a_large_read_is_refused_not_slowed() {
    let footprint = Arc::new(AtomicUsize::new(100 * MIB));
    let probe = footprint.clone();
    let limits = ReadLimits::new(
        CursorLimits::PRODUCTION,
        ReadBudget::with_probe(Some(120 * MIB), move || Some(probe.load(Ordering::SeqCst))),
    );
    let h = connection(&limits);

    // 100 MiB resident + 48 MiB headroom for a new build > 120 MiB.
    let refused = handle_json(&h, &request(None, PAGE, 1)).unwrap();
    assert!(is_error(&refused));
    assert_eq!(error_code(&refused), "memory_budget_exceeded");
    assert!(refused.contains("memory_budget_exceeded"));
    // Refusal keeps no snapshot and holds no reservation.
    assert_eq!(limits.pool().totals(), (0, 0));
    assert_eq!(limits.budget().reserved(), 0);

    // Memory drops back: the same read is served again.
    footprint.store(40 * MIB, Ordering::SeqCst);
    let served = handle_json(&h, &request(None, PAGE, 2)).unwrap();
    assert!(!is_error(&served), "{served}");
    assert_eq!(
        limits.budget().reserved(),
        0,
        "reservation is released with the response"
    );
}

#[test]
fn a_page_is_refused_when_its_working_set_does_not_fit() {
    // Room for the build headroom, not for 4x a 1.8 MiB snapshot on top of
    // the resident size.
    let footprint = Arc::new(AtomicUsize::new(0));
    let probe = footprint.clone();
    let one = snapshot_bytes();
    let limits = ReadLimits::new(
        CursorLimits::PRODUCTION,
        ReadBudget::with_probe(Some(150 * MIB), move || Some(probe.load(Ordering::SeqCst))),
    );
    let h = connection(&limits);
    let cursor = first_page(&h);
    footprint.store(150 * MIB - 4 * one + MIB, Ordering::SeqCst);
    let out = handle_json(&h, &request(Some(&cursor), PAGE, 2)).unwrap();
    assert_eq!(error_code(&out), "memory_budget_exceeded", "{out}");
    // The cursor is still there once memory is available.
    footprint.store(10 * MIB, Ordering::SeqCst);
    let out = handle_json(&h, &request(Some(&cursor), PAGE, 3)).unwrap();
    assert!(!is_error(&out), "{out}");
}

#[test]
fn an_unlimited_cgroup_never_refuses() {
    let budget = ReadBudget::with_probe(None, || Some(usize::MAX / 2));
    let limits = ReadLimits::new(CursorLimits::PRODUCTION, budget);
    let h = connection(&limits);
    first_page(&h);
}
