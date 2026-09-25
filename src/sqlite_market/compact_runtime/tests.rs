use std::fs;
use std::io::{BufRead, BufReader, Write as _};
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::*;
use rusqlite::params;

const CHILD_MODE: &str = "X_DATA_MCP_S4_CHILD_MODE";
const CHILD_DB: &str = "X_DATA_MCP_S4_CHILD_DB";

fn tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir_in("/dev/shm")
        .unwrap()
}

fn create_db(path: &Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY, checksum TEXT NOT NULL) STRICT;
         INSERT INTO schema_migrations(version,checksum) VALUES(1,'abc');
         CREATE TABLE receipts(id INTEGER PRIMARY KEY, payload BLOB) STRICT;
         PRAGMA user_version=1;",
    )
    .unwrap();
    conn.close().unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn hex_digest(text: &str) -> Digest {
    let mut digest = [0u8; 32];
    for index in 0..32 {
        digest[index] = u8::from_str_radix(&text[2 * index..2 * index + 2], 16).unwrap();
    }
    digest
}

fn expected_identity() -> SchemaIdentity {
    SchemaIdentity {
        user_version: 1,
        digest: hex_digest("57dbc4cb075c311c233eeee24fdd44c71b1ad5e1fdc51ef73d51188bc82fdf55"),
    }
}

fn coordinator(path: &Path) -> WriteCoordinator {
    let lease = Arc::new(DatabaseLease::acquire(path).unwrap());
    WriteCoordinator::open_existing(path, lease, expected_identity(), Duration::from_secs(30))
        .unwrap()
}

#[test]
fn open_boundary_and_writer_invariants() {
    let dir = tempdir();
    let path = dir.path().join("market.db");
    create_db(&path);
    let identity = inspect_schema_identity(&Connection::open(&path).unwrap()).unwrap();
    assert_eq!(identity, expected_identity());
    let lease = Arc::new(DatabaseLease::acquire(&path).unwrap());
    let mut wrong = expected_identity();
    wrong.digest = [9; 32];
    assert!(
        WriteCoordinator::open_existing(&path, lease.clone(), wrong, Duration::from_secs(5))
            .is_err()
    );
    let absent = dir.path().join("absent.db");
    assert!(DatabaseLease::acquire(&absent).is_err());
    assert!(
        Connection::open_with_flags(
            &absent,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX
        )
        .is_err()
    );
    assert!(!absent.exists());
    let coordinator =
        WriteCoordinator::open_existing(&path, lease, expected_identity(), Duration::from_secs(30))
            .unwrap();
    coordinator
        .write(|conn| {
            conn.execute_batch("CREATE TEMP TABLE probe(x); INSERT INTO probe VALUES(1)")
                .map_err(storage)?;
            Ok(())
        })
        .unwrap();
    coordinator
        .write(|conn| {
            let count: i64 = conn
                .query_row("SELECT count(*) FROM probe", [], |r| r.get(0))
                .map_err(storage)?;
            assert_eq!(count, 1);
            Ok(())
        })
        .unwrap();
    for index in 0..3i64 {
        coordinator
            .transaction(|tx| {
                let mut statement = tx
                    .prepare_cached("INSERT INTO receipts(id,payload) VALUES(?1,?2)")
                    .map_err(storage)?;
                statement
                    .execute(params![index, vec![0u8; 8]])
                    .map_err(storage)?;
                Ok(())
            })
            .unwrap();
    }
    coordinator
        .write(|conn| {
            let journal: String = conn
                .pragma_query_value(None, "journal_mode", |r| r.get(0))
                .map_err(storage)?;
            assert_eq!(journal, "wal");
            let synchronous: i64 = conn
                .pragma_query_value(None, "synchronous", |r| r.get(0))
                .map_err(storage)?;
            assert_eq!(synchronous, 2);
            let foreign_keys: i64 = conn
                .pragma_query_value(None, "foreign_keys", |r| r.get(0))
                .map_err(storage)?;
            assert_eq!(foreign_keys, 1);
            let autocheckpoint: i64 = conn
                .pragma_query_value(None, "wal_autocheckpoint", |r| r.get(0))
                .map_err(storage)?;
            assert_eq!(autocheckpoint, 1000);
            let limit: i64 = conn
                .pragma_query_value(None, "journal_size_limit", |r| r.get(0))
                .map_err(storage)?;
            assert_eq!(limit, 16_777_216);
            Ok(())
        })
        .unwrap();
    assert!(matches!(
        coordinator.write(|conn| {
            conn.execute_batch("BEGIN").map_err(storage)?;
            Ok(())
        }),
        Err(Error::Integrity(_))
    ));
    coordinator
        .write(|conn| {
            assert!(conn.is_autocommit());
            Ok(())
        })
        .unwrap();
    assert!(
        coordinator
            .write(|conn| {
                conn.execute_batch("UPDATE schema_migrations SET checksum='tampered'")
                    .map_err(storage)?;
                Ok(())
            })
            .is_err()
    );
    assert!(coordinator.write(|_| Ok(())).is_err());
}

#[test]
fn concurrent_readers_bounded() {
    let dir = tempdir();
    let path = dir.path().join("market.db");
    create_db(&path);
    let coordinator = coordinator(&path);
    let active = AtomicUsize::new(0);
    let maximum = AtomicUsize::new(0);
    let completed = AtomicUsize::new(0);
    thread::scope(|scope| {
        for _ in 0..32 {
            scope.spawn(|| {
                coordinator
                    .read(|conn| {
                        let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                        maximum.fetch_max(now, Ordering::SeqCst);
                        thread::sleep(Duration::from_millis(40));
                        conn.query_row("SELECT count(*) FROM receipts", [], |r| r.get::<_, i64>(0))
                            .map_err(storage)?;
                        active.fetch_sub(1, Ordering::SeqCst);
                        completed.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    })
                    .unwrap();
            });
        }
    });
    assert!(maximum.load(Ordering::SeqCst) <= 16);
    assert_eq!(completed.load(Ordering::SeqCst), 32);
}

#[test]
fn wal_bounded_under_mixed_load() {
    let dir = tempdir();
    let path = dir.path().join("market.db");
    create_db(&path);
    let coordinator = coordinator(&path);
    let stop = AtomicBool::new(false);
    let mut sizes = Vec::new();
    thread::scope(|scope| {
        for _ in 0..16 {
            scope.spawn(|| {
                while !stop.load(Ordering::SeqCst) {
                    coordinator
                        .read(|conn| {
                            conn.query_row("SELECT count(*) FROM receipts", [], |r| {
                                r.get::<_, i64>(0)
                            })
                            .map_err(storage)?;
                            Ok(())
                        })
                        .unwrap();
                }
            });
        }
        for index in 0..120i64 {
            coordinator
                .transaction(|tx| {
                    tx.execute(
                        "INSERT INTO receipts(id,payload) VALUES(?1,?2)",
                        params![index, vec![7u8; 8192]],
                    )
                    .map_err(storage)?;
                    Ok(())
                })
                .unwrap();
            if index % 15 == 14 {
                sizes.push(coordinator.checkpoint_passive().unwrap().wal_bytes_after);
            }
        }
        stop.store(true, Ordering::SeqCst);
    });
    let final_metrics = coordinator.checkpoint_passive().unwrap();
    let idle_metrics = coordinator.checkpoint_idle().unwrap();
    coordinator
        .transaction(|tx| {
            tx.execute("INSERT INTO receipts(id,payload) VALUES(120,x'09')", [])
                .map_err(storage)?;
            Ok(())
        })
        .unwrap();
    let wal_max = sizes.iter().copied().max().unwrap_or(0);
    let non_increase = sizes.windows(2).filter(|pair| pair[1] <= pair[0]).count();
    println!(
        "S4_WAL_METRICS={}",
        serde_json::json!({
            "max_reader_slots": 16,
            "writer_transactions": 121,
            "wal_samples": sizes,
            "wal_max_bytes": wal_max,
            "non_increase_points": non_increase,
            "final_busy": final_metrics.busy,
            "final_log_frames": final_metrics.log_frames,
            "final_checkpointed_frames": final_metrics.checkpointed_frames,
            "final_wal_bytes_after": final_metrics.wal_bytes_after,
            "idle_mode": "truncate",
            "idle_busy": idle_metrics.busy,
            "idle_log_frames": idle_metrics.log_frames,
            "idle_checkpointed_frames": idle_metrics.checkpointed_frames,
            "idle_wal_bytes_after": idle_metrics.wal_bytes_after,
        })
    );
    assert!(wal_max <= 32 * 1024 * 1024);
    assert!(non_increase >= 1);
    assert_eq!(final_metrics.busy, 0);
    assert_eq!(final_metrics.checkpointed_frames, final_metrics.log_frames);
    assert_eq!(idle_metrics.mode, CheckpointMode::Truncate);
    assert_eq!(idle_metrics.busy, 0);
    assert_eq!(idle_metrics.wal_bytes_after, 0);
    let shutdown = coordinator.shutdown().unwrap();
    assert_eq!(shutdown.mode, CheckpointMode::Truncate);
    assert_eq!(shutdown.busy, 0);
    assert_eq!(shutdown.wal_bytes_after, 0);
    drop(coordinator);
    let conn = Connection::open(&path).unwrap();
    let rows: i64 = conn
        .query_row("SELECT count(*) FROM receipts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 121);
}

#[test]
fn shutdown_waits_for_reader_then_truncates() {
    let dir = tempdir();
    let path = dir.path().join("market.db");
    create_db(&path);
    let coordinator = Arc::new(coordinator(&path));
    coordinator
        .transaction(|tx| {
            tx.execute("INSERT INTO receipts(id,payload) VALUES(1,x'aa')", [])
                .map_err(storage)?;
            Ok(())
        })
        .unwrap();
    let (entered_send, entered_recv) = mpsc::channel::<()>();
    let (release_send, release_recv) = mpsc::channel::<()>();
    let reading = coordinator.clone();
    let reader = thread::spawn(move || {
        reading
            .read(|conn| {
                entered_send.send(()).unwrap();
                release_recv.recv().unwrap();
                conn.query_row("SELECT count(*) FROM receipts", [], |r| r.get::<_, i64>(0))
                    .map_err(storage)?;
                Ok(())
            })
            .unwrap();
    });
    entered_recv.recv().unwrap();
    let closing = coordinator.clone();
    let shutdown_thread = thread::spawn(move || closing.shutdown());
    thread::sleep(Duration::from_millis(300));
    assert!(!shutdown_thread.is_finished());
    release_send.send(()).unwrap();
    reader.join().unwrap();
    let metrics = shutdown_thread.join().unwrap().unwrap();
    assert_eq!(metrics.mode, CheckpointMode::Truncate);
    assert_eq!(metrics.busy, 0);
    assert_eq!(metrics.wal_bytes_after, 0);
    assert!(coordinator.read(|_| Ok(())).is_err());
    assert!(coordinator.write(|_| Ok(())).is_err());
    assert!(coordinator.transaction(|_| Ok(())).is_err());
    assert!(coordinator.checkpoint_passive().is_err());
    assert!(coordinator.checkpoint_idle().is_err());
    assert!(coordinator.shutdown().is_err());
    drop(coordinator);
    let conn = Connection::open(&path).unwrap();
    let rows: i64 = conn
        .query_row("SELECT count(*) FROM receipts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 1);
    let check: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(check, "ok");
}

#[test]
fn sigkill_recovery_preserves_committed_work() {
    if let Ok(mode) = std::env::var(CHILD_MODE) {
        let path = PathBuf::from(std::env::var(CHILD_DB).unwrap());
        let coordinator = coordinator(&path);
        match mode.as_str() {
            "committed" => {
                coordinator
                    .transaction(|tx| {
                        tx.execute(
                            "INSERT INTO receipts(id,payload) VALUES(1,?1)",
                            params![vec![1u8; 64]],
                        )
                        .map_err(storage)?;
                        Ok(())
                    })
                    .unwrap();
                println!("S4_READY");
                std::io::stdout().flush().unwrap();
                thread::sleep(Duration::from_secs(300));
            }
            "uncommitted" => {
                coordinator
                    .write(|conn| {
                        let tx = conn
                            .transaction_with_behavior(TransactionBehavior::Immediate)
                            .map_err(storage)?;
                        tx.execute(
                            "INSERT INTO receipts(id,payload) VALUES(2,?1)",
                            params![vec![2u8; 64]],
                        )
                        .map_err(storage)?;
                        println!("S4_READY");
                        std::io::stdout().flush().unwrap();
                        thread::sleep(Duration::from_secs(300));
                        drop(tx);
                        Ok(())
                    })
                    .unwrap();
            }
            other => panic!("unknown child mode {other}"),
        }
        return;
    }
    let dir = tempdir();
    let path = dir.path().join("market.db");
    create_db(&path);
    for mode in ["committed", "uncommitted"] {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "sqlite_market::compact_runtime::tests::sigkill_recovery_preserves_committed_work",
                "--nocapture",
            ])
            .env(CHILD_MODE, mode)
            .env(CHILD_DB, &path)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        loop {
            line.clear();
            if stdout.read_line(&mut line).unwrap() == 0 {
                panic!("{mode} child exited without READY");
            }
            if line.contains("S4_READY") {
                break;
            }
        }
        child.kill().unwrap();
        let status = child.wait().unwrap();
        assert!(!status.success());
    }
    let coordinator = coordinator(&path);
    let ids: Vec<i64> = coordinator
        .read(|conn| {
            let mut statement = conn
                .prepare("SELECT id FROM receipts ORDER BY id")
                .map_err(storage)?;
            let ids = statement
                .query_map([], |r| r.get::<_, i64>(0))
                .map_err(storage)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(storage)?;
            Ok(ids)
        })
        .unwrap();
    assert_eq!(ids, vec![1]);
    let check: String = coordinator
        .read(|conn| {
            conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))
                .map_err(storage)
        })
        .unwrap();
    assert_eq!(check, "ok");
    let metrics = coordinator.shutdown().unwrap();
    assert_eq!(metrics.mode, CheckpointMode::Truncate);
    assert_eq!(metrics.busy, 0);
    assert_eq!(metrics.wal_bytes_after, 0);
}

#[test]
fn lease_mismatch_and_inode_swap_fail_closed() {
    let dir = tempdir();
    let first = dir.path().join("market.db");
    let second = dir.path().join("other.db");
    create_db(&first);
    create_db(&second);
    let lease = Arc::new(DatabaseLease::acquire(&first).unwrap());
    assert!(
        WriteCoordinator::open_existing(
            &second,
            lease.clone(),
            expected_identity(),
            Duration::from_secs(5)
        )
        .is_err()
    );
    let coordinator = WriteCoordinator::open_existing(
        &first,
        lease,
        expected_identity(),
        Duration::from_secs(30),
    )
    .unwrap();
    let moved = dir.path().join("market.db.orig");
    fs::rename(&first, &moved).unwrap();
    fs::rename(&second, &first).unwrap();
    assert!(coordinator.write(|_| Ok(())).is_err());
    assert!(coordinator.read(|_| Ok(())).is_err());
    assert!(coordinator.checkpoint_passive().is_err());
}

#[test]
fn read_detects_path_swap_during_closure() {
    let dir = tempdir();
    let first = dir.path().join("market.db");
    let second = dir.path().join("other.db");
    create_db(&first);
    create_db(&second);
    let coordinator = Arc::new(coordinator(&first));
    let (entered_send, entered_recv) = mpsc::channel::<()>();
    let (release_send, release_recv) = mpsc::channel::<()>();
    let reading = coordinator.clone();
    let reader = thread::spawn(move || {
        reading.read(|conn| {
            entered_send.send(()).unwrap();
            release_recv.recv().unwrap();
            conn.query_row("SELECT count(*) FROM receipts", [], |r| r.get::<_, i64>(0))
                .map_err(storage)?;
            Ok(())
        })
    });
    entered_recv.recv().unwrap();
    let moved = dir.path().join("market.db.orig");
    fs::rename(&first, &moved).unwrap();
    fs::rename(&second, &first).unwrap();
    release_send.send(()).unwrap();
    assert!(reader.join().unwrap().is_err());
}

/// Real-DB connection-churn probe: times each fixed-cost layer of
/// `coordinator.read`/`write` on a *copy* of the production database.
///
///   X_DATA_PROBE_DB=/path/copy.db   required; aborts when unset
#[test]
#[ignore = "manual real-DB probe; needs X_DATA_PROBE_DB=<copy>"]
fn coordinator_boundary_cost_probe() {
    use std::time::Instant;
    let Ok(db) = std::env::var("X_DATA_PROBE_DB") else {
        eprintln!("skipped: X_DATA_PROBE_DB unset");
        return;
    };
    let path = Path::new(&db);
    let n = 200;
    let report = |label: &str, mut ms: Vec<f64>| {
        ms.sort_by(f64::total_cmp);
        eprintln!(
            "  {label}: n={} p50={:.3}ms mean={:.3}ms p90={:.3}ms",
            ms.len(),
            ms[ms.len() / 2],
            ms.iter().sum::<f64>() / ms.len() as f64,
            ms[ms.len() * 9 / 10]
        );
    };

    // Layer 1: bare open + busy_timeout only.
    let mut ms = Vec::new();
    for _ in 0..n {
        let t = Instant::now();
        let c = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .unwrap();
        c.busy_timeout(Duration::from_secs(5)).unwrap();
        drop(c);
        ms.push(t.elapsed().as_secs_f64() * 1e3);
    }
    report("open_only", ms);

    // Layer 2: open + full read() preamble pragmas.
    let mut ms = Vec::new();
    for _ in 0..n {
        let t = Instant::now();
        let c = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .unwrap();
        c.busy_timeout(Duration::from_secs(5)).unwrap();
        set_pragma(&c, "foreign_keys", 1).unwrap();
        set_pragma(&c, "query_only", 1).unwrap();
        verify_wal(&c).unwrap();
        drop(c);
        ms.push(t.elapsed().as_secs_f64() * 1e3);
    }
    report("open+pragmas", ms);

    // Layer 3: inspect_schema_identity on a held connection.
    let held = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap();
    let mut ms = Vec::new();
    for _ in 0..n {
        let t = Instant::now();
        inspect_schema_identity(&held).unwrap();
        ms.push(t.elapsed().as_secs_f64() * 1e3);
    }
    report("inspect_schema_identity", ms);
    drop(held);

    // Layer 4: full coordinator.read / write on the real schema.
    let lease = Arc::new(DatabaseLease::acquire(path).unwrap());
    let coord = WriteCoordinator::open_existing(
        path,
        lease,
        crate::sqlite_market::compact_repo::schema19_identity(),
        Duration::from_secs(5),
    )
    .unwrap();
    let mut ms = Vec::new();
    for _ in 0..n {
        let t = Instant::now();
        coord.read(|_| Ok(())).unwrap();
        ms.push(t.elapsed().as_secs_f64() * 1e3);
    }
    report("coordinator.read(empty)", ms);
    let mut ms = Vec::new();
    for _ in 0..n {
        let t = Instant::now();
        coord.write(|_| Ok(())).unwrap();
        ms.push(t.elapsed().as_secs_f64() * 1e3);
    }
    report("coordinator.write(empty)", ms);
    let mut ms = Vec::new();
    for _ in 0..n {
        let t = Instant::now();
        coord
            .transaction(|_| -> Result<(), Error> { Ok(()) })
            .unwrap();
        ms.push(t.elapsed().as_secs_f64() * 1e3);
    }
    report("coordinator.tx(empty)", ms);
}
