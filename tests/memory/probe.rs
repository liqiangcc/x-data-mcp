//! Measurement probe, not a regression gate: run with
//! `cargo test --test memory probe -- --ignored --nocapture --test-threads=1`.
use super::fake::*;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicUsize, Ordering::Relaxed},
};
use x_data_mcp::{
    data_supply::{
        cursor_cache::CursorLimits,
        query::Queries,
        read_limits::{ReadBudget, ReadLimits},
    },
    mcp::handle_json,
};

pub struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            let now = LIVE.fetch_add(l.size(), Relaxed) + l.size();
            PEAK.fetch_max(now, Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Relaxed);
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, l, n) };
        if !q.is_null() {
            let now = if n >= l.size() {
                LIVE.fetch_add(n - l.size(), Relaxed) + (n - l.size())
            } else {
                LIVE.fetch_sub(l.size() - n, Relaxed) - (l.size() - n)
            };
            PEAK.fetch_max(now, Relaxed);
        }
        q
    }
}
#[global_allocator]
static A: Counting = Counting;

fn status(key: &str) -> usize {
    let s = std::fs::read_to_string("/proc/self/status").unwrap();
    s.lines()
        .find(|l| l.starts_with(key))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0)
        * 1024
}
fn hwm() -> usize {
    status("VmHWM")
}
fn mib(n: usize) -> f64 {
    n as f64 / 1048576.0
}
fn report(tag: &str) {
    let info = unsafe { libc::mallinfo2() };
    println!(
        "{tag:<34} live={:7.1} peak={:7.1} rss_anon={:7.1} rss_hwm={:7.1} arena={:7.1} in_use={:7.1} mmap={:6.1} (MiB)",
        mib(LIVE.load(Relaxed)),
        mib(PEAK.load(Relaxed)),
        mib(status("RssAnon")),
        mib(hwm()),
        mib(info.arena),
        mib(info.uordblks),
        mib(info.hblkhd)
    );
}

#[test]
#[ignore]
fn probe_universe_memory() {
    let entities: usize = std::env::var("PROBE_ENTITIES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5225);
    let page: usize = std::env::var("PROBE_PAGE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1000);
    let conns: usize = std::env::var("PROBE_CONNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let trim = std::env::var("PROBE_TRIM").is_ok();
    let each_trim = std::env::var("PROBE_TRIM_EACH").is_ok();
    if let Ok(v) = std::env::var("PROBE_ARENA_MAX") {
        assert_eq!(
            unsafe { libc::mallopt(libc::M_ARENA_MAX, v.parse().unwrap()) },
            1
        );
    }
    if let Ok(v) = std::env::var("PROBE_MMAP_THRESHOLD") {
        assert_eq!(
            unsafe { libc::mallopt(libc::M_MMAP_THRESHOLD, v.parse().unwrap()) },
            1
        );
    }
    if let Ok(v) = std::env::var("PROBE_TRIM_THRESHOLD") {
        assert_eq!(
            unsafe { libc::mallopt(libc::M_TRIM_THRESHOLD, v.parse().unwrap()) },
            1
        );
    }
    report("baseline");
    // Each "connection" owns its handler on its own thread, as in
    // local_runtime::server (`factory(entry)` runs inside `thread::spawn`).
    let (hold_tx, hold_rx) = std::sync::mpsc::channel::<()>();
    let hold_rx = std::sync::Mutex::new(hold_rx);
    let barrier = std::sync::Barrier::new(conns + 1);
    // PROBE_SHARED=1: one process-wide pool, as `data_service` now builds it.
    // PROBE_PRODUCTION=1: exactly what `data_service` builds (cgroup budget).
    let shared = if std::env::var("PROBE_PRODUCTION").is_ok() {
        Some(ReadLimits::production())
    } else {
        std::env::var("PROBE_SHARED")
            .is_ok()
            .then(|| ReadLimits::new(CursorLimits::PRODUCTION, ReadBudget::unlimited()))
    };
    let refused = AtomicUsize::new(0);
    let served = AtomicUsize::new(0);
    let expired = AtomicUsize::new(0);
    let server_trim = std::env::var("PROBE_SERVER_TRIM").is_ok();
    let mut first = vec![];
    let mut stats = (0, 0, 0);
    std::thread::scope(|s| {
        let joins: Vec<_> = (0..conns)
            .map(|_| {
                let barrier = &barrier;
                let hold_rx = &hold_rx;
                let shared = &shared;
                let (refused, served, expired) = (&refused, &served, &expired);
                s.spawn(move || {
                    let h = Handler(match shared {
                        Some(limits) => Queries::with_limits(Fake { entities }, limits),
                        None => Queries::new(Fake { entities }),
                    });
                    let mut sizes = vec![];
                    let mut cursor: Option<String> = None;
                    loop {
                        PEAK.store(LIVE.load(Relaxed), Relaxed);
                        let out = handle_json(&h, &request(cursor.as_deref(), page, 1)).unwrap();
                        sizes.push((out.len(), PEAK.load(Relaxed)));
                        if is_error(&out) {
                            // Either the read budget refused, or the shared
                            // pool evicted this connection's cursor.
                            if error_code(&out) == "memory_budget_exceeded" {
                                refused.fetch_add(1, Relaxed);
                            } else {
                                assert!(out.contains("invalid/expired cursor"), "{out}");
                                expired.fetch_add(1, Relaxed);
                            }
                            break;
                        }
                        served.fetch_add(1, Relaxed);
                        // What local_runtime::server does after a large response.
                        if server_trim && out.len() >= x_data_mcp::memory::TRIM_AFTER_RESPONSE_BYTES
                        {
                            drop(std::mem::take(&mut cursor));
                            let next = next_cursor(&out);
                            drop(out);
                            x_data_mcp::memory::release_free_memory();
                            cursor = next;
                            if cursor.is_none() {
                                break;
                            }
                            continue;
                        }
                        if each_trim {
                            unsafe { libc::malloc_trim(0) };
                        }
                        cursor = next_cursor(&out);
                        if cursor.is_none() {
                            break;
                        }
                    }
                    let st = h.0.cache_stats();
                    barrier.wait(); // all connections finished, caches still held
                    let _ = hold_rx
                        .lock()
                        .unwrap()
                        .recv_timeout(std::time::Duration::from_secs(30));
                    (sizes, st)
                })
            })
            .collect();
        barrier.wait();
        report("all queries done, conns open");
        if let Some(limits) = &shared {
            println!(
                "shared pool (entries, bytes) = {:?}",
                limits.pool().totals()
            );
        }
        for _ in 0..conns {
            hold_tx.send(()).unwrap();
        }
        for (i, j) in joins.into_iter().enumerate() {
            let (sizes, st) = j.join().unwrap();
            if i == 0 {
                first = sizes;
                stats = st;
            }
        }
    });
    for (n, (len, peak)) in first.iter().enumerate() {
        println!(
            "conn0 page{n}: response={:.2} MiB peak_live_during={:.1} MiB",
            mib(*len),
            mib(*peak)
        );
    }
    println!("cache stats (entries, bytes, snapshots) conn0 = {stats:?}");
    println!(
        "pages served={} refused={}",
        served.load(Relaxed),
        refused.load(Relaxed)
    );
    report("after connections closed");
    if trim {
        unsafe { libc::malloc_trim(0) };
        report("after malloc_trim(0)");
    }
}

/// Where does the per-page transient go? Heap deltas of the pieces
/// `query_page` and the columnar projection materialise for one page.
#[test]
#[ignore]
fn probe_page_pieces() {
    use x_data_mcp::data_supply::{
        Access,
        query::{QueryPlan, ReadRepository},
        wire::*,
    };
    let fake = Fake { entities: 5225 };
    let args: Query = serde_json::from_str(&request(None, 1000, 1))
        .map(|v: serde_json::Value| {
            serde_json::from_value(v["params"]["arguments"].clone()).unwrap()
        })
        .unwrap();
    let access = Access::local();
    let mut plan = QueryPlan::new(args, &access).unwrap();
    plan.resolve(&fake, &access).unwrap();
    let before = LIVE.load(Relaxed);
    let snap = fake.query(&plan, &access).unwrap();
    let snap_heap = LIVE.load(Relaxed) - before;
    let ser = serde_json::to_vec(&snap).unwrap().len();
    let b = LIVE.load(Relaxed);
    let manifest = snap.manifest.clone();
    let manifest_heap = LIVE.load(Relaxed) - b;
    let b = LIVE.load(Relaxed);
    let rows = snap.rows[..1000].to_vec();
    let rows_heap = LIVE.load(Relaxed) - b;
    println!(
        "snapshot: serialized={:.2} MiB heap={:.2} MiB (x{:.2})",
        mib(ser),
        mib(snap_heap),
        snap_heap as f64 / ser as f64
    );
    println!(
        "manifest.clone() (5225 entries): {:.2} MiB; rows[..1000].to_vec(): {:.2} MiB",
        mib(manifest_heap),
        mib(rows_heap)
    );
    drop((manifest, rows));
}

/// Transient heap of ONE request (page 0 and page 1) against entity count and
/// page size: `peak - live_before`, cache excluded for page 1.
#[test]
#[ignore]
fn probe_transient_model() {
    for (entities, page) in [
        (1000usize, 100usize),
        (5225, 1000),
        (8000, 1000),
        (8800, 1000),
        (9300, 1000),
    ] {
        let h = Handler(Queries::new(Fake { entities }));
        let b0 = LIVE.load(Relaxed);
        PEAK.store(b0, Relaxed);
        let out = handle_json(&h, &request(None, page, 1)).unwrap();
        let p0 = PEAK.load(Relaxed) - b0;
        let resp0 = out.len();
        let cursor = next_cursor(&out);
        let held = LIVE.load(Relaxed) - b0; // cache + `out` still alive
        let (_, ser, _) = h.0.cache_stats();
        let mut p1 = 0;
        let mut resp1 = 0;
        if let Some(c) = cursor {
            drop(out);
            let b1 = LIVE.load(Relaxed);
            PEAK.store(b1, Relaxed);
            let o = handle_json(&h, &request(Some(&c), page, 1)).unwrap();
            p1 = PEAK.load(Relaxed) - b1;
            resp1 = o.len();
        }
        println!(
            "entities={entities:6} page={page:5} | page0: transient={:6.2} MiB resp={:5.2} | cache(est bytes)={:6.2} held_after_p0={:6.2} | page1: transient={:6.2} MiB resp={:5.2}",
            mib(p0),
            mib(resp0),
            mib(ser),
            mib(held),
            mib(p1),
            mib(resp1)
        );
    }
}

/// Heap held by each stage of one response, page 0 of the full universe.
#[test]
#[ignore]
fn probe_response_stages() {
    use x_data_mcp::data_supply::Access;
    let h = Queries::new(Fake { entities: 5225 });
    let args: x_data_mcp::data_supply::wire::Query = {
        let v: serde_json::Value = serde_json::from_str(&request(None, 1000, 1)).unwrap();
        serde_json::from_value(v["params"]["arguments"].clone()).unwrap()
    };
    let b = LIVE.load(Relaxed);
    let value = h.query_v2(args, &Access::local()).unwrap();
    let value_heap = LIVE.load(Relaxed) - b - {
        let (_, ser, _) = h.cache_stats();
        (ser as f64 * 1.45) as usize
    };
    let text = serde_json::to_string(&value).unwrap();
    let b = LIVE.load(Relaxed);
    let response = serde_json::json!({"result": {"content": [{"type": "text", "text": text.clone()}], "structuredContent": value}});
    let _ = b;
    let b = LIVE.load(Relaxed);
    let wire = serde_json::to_string(&response).unwrap();
    let wire_heap = LIVE.load(Relaxed) - b;
    println!(
        "structuredContent JSON={:.2} MiB; Value tree heap~={:.2} MiB; text copy={:.2} MiB; final wire string={:.2} MiB (alloc {:.2})",
        mib(text.len()),
        mib(value_heap),
        mib(text.len()),
        mib(wire.len()),
        mib(wire_heap)
    );
}
