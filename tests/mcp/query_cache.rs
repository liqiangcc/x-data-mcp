use serde_json::json;
use x_data_mcp::{
    data_supply::{
        Access,
        query::Queries,
        wire::{Filter, Query},
    },
    sqlite_market::SqliteMarket,
    sqlite_supply::SqliteSupply,
    sqlite_tasks::SqliteTasks,
};

use crate::support_market as market;

fn args() -> Query {
    serde_json::from_value(json!({"dataset":"market.daily","contract_version":"v1","selection":{"kind":"single","entity":{"exchange":"sh","code":"600006"}},"filter":{"kind":"daily","start_date":"2025-01-01","end_date":"2025-12-31"},"version":{"kind":"latest_per_entity"},"page_size":1})).unwrap()
}

/// Paging one query mints several cursors over a single shared snapshot;
/// the byte-bounded LRU evicts least-recently-used snapshots, and an
/// evicted cursor reads invalid/expired while survivors still page.
#[test]
fn paged_query_shares_one_snapshot_and_byte_cap_evicts_lru() {
    let d = tempfile::tempdir().unwrap();
    let t = SqliteTasks::new(d.path().join("db"));
    t.initialize().unwrap();
    let store = SqliteMarket::new(t);
    market::published_fixture(&store, &market::samples()[0]["record"]);
    let repo = SqliteSupply {
        market: x_data_mcp::sqlite_market::backend::MarketBackend::Legacy(store),
    };
    let access = Access::local();

    // Three in-window rows at page_size 1 -> three pages, two cursors.
    let wide = || {
        let mut a = args();
        let Filter::Daily { end_date, .. } = a.filter else {
            panic!()
        };
        a.filter = Filter::Daily {
            start_date: "2024-12-01".parse().unwrap(),
            end_date,
            as_of: None,
        };
        a
    };
    let q = Queries::new(repo.clone());
    let first = q.query(wide(), &access).unwrap();
    let (_, snapshot_bytes, _) = q.cache_stats();
    let mut next = wide();
    next.cursor = first.next_cursor.clone();
    let second = q.query(next, &access).unwrap();
    let mut tail = wide();
    tail.cursor = second.next_cursor.clone();
    let third = q.query(tail, &access).unwrap();
    assert!(third.next_cursor.is_none() && third.page_complete);
    assert_eq!(first.snapshot_id, third.snapshot_id);
    assert_eq!(first.query_time, third.query_time);
    // Two cursor entries, one shared snapshot: bytes are accounted once.
    assert_eq!(q.cache_stats(), (2, snapshot_bytes, 1));

    // Distinct queries (same 2-row page, different end_date -> different
    // key) each hold one snapshot of `one_snapshot` bytes; the cap below
    // admits at most two.
    let args_at = |end: &str| {
        let mut a = args();
        let Filter::Daily { start_date, .. } = a.filter else {
            panic!()
        };
        a.filter = Filter::Daily {
            start_date,
            end_date: end.parse().unwrap(),
            as_of: None,
        };
        a
    };
    let probe = Queries::new(repo.clone());
    probe.query(args_at("2026-01-31"), &access).unwrap();
    let (_, one_snapshot, _) = probe.cache_stats();
    let q = Queries::with_cache_limits(repo, 32, one_snapshot * 5 / 2);
    let mut cursors = Vec::new();
    for end in ["2026-01-31", "2026-02-28"] {
        cursors.push(q.query(args_at(end), &access).unwrap().next_cursor.unwrap());
    }
    // Resuming the oldest cursor makes it most-recently-used.
    let mut resumed = args_at("2026-01-31");
    resumed.cursor = Some(cursors[0].clone());
    q.query(resumed, &access).unwrap();
    cursors.push(
        q.query(args_at("2026-03-31"), &access)
            .unwrap()
            .next_cursor
            .unwrap(),
    );
    // The never-resumed second query was evicted, not the resumed first.
    let mut evicted = args_at("2026-02-28");
    evicted.cursor = Some(cursors[1].clone());
    assert!(q.query(evicted, &access).is_err());
    let mut again = args_at("2026-01-31");
    again.cursor = Some(cursors[0].clone());
    assert!(q.query(again, &access).unwrap().page_complete);
    // The second resume re-marks the first snapshot MRU, so the next push
    // evicts the untouched third one instead.
    q.query(args_at("2026-04-30"), &access).unwrap();
    let mut lru_evicted = args_at("2026-03-31");
    lru_evicted.cursor = Some(cursors[2].clone());
    assert!(q.query(lru_evicted, &access).is_err());
    // One more push finally evicts the twice-resumed first snapshot.
    q.query(args_at("2026-05-31"), &access).unwrap();
    let (_, bytes, distinct) = q.cache_stats();
    assert!(bytes <= one_snapshot * 5 / 2);
    assert_eq!(distinct, 2);
    let mut gone = args_at("2026-01-31");
    gone.cursor = Some(cursors[0].clone());
    assert!(q.query(gone, &access).is_err());
}
