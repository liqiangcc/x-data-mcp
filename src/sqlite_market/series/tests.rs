//! SPEC-LATEST-ONLY §8.1 checks for the schema-21 latest-only store.
//! Covered: fold equivalence + path independence (1), content-derived
//! version_id + DDL CHECK (2), event/series closure + tamper cases (3,4),
//! first_publish replay anchor (4), stale_version vs version_not_found (9),
//! read paths through `SqliteSupply` (versions/query/availability/presence),
//! yearly stale classification (A1), and an `#[ignore]`d 1M-event startup
//! timing fixture (11).

use super::*;
use crate::compact_market::{EvidenceKind, SourceBinding, VersionEvidence};
use crate::data_supply::Access;
use crate::data_supply::observe::ObservationRepository;
use crate::data_supply::presence::PresenceRepository;
use crate::data_supply::query::{QueryPlan, ReadRepository};
use crate::data_supply::wire::{
    Dataset, Exchange, Filter, Query, Security, Selection, VersionBinding, VersionChoice,
    VersionsQuery, YearPresenceItem, YearPresenceQuery,
};
use crate::market_data::{AdjustmentEvidence, UnitCurrencyEvidence};
use crate::sqlite_tasks::SqliteTasks;
use chrono::NaiveDate;
use rusqlite::{Connection, params};

const ENTITY: &str = "sh.600006";
/// Schema 22's only materialized shape (forward-adjusted stock, 分).
const SHAPE: SeriesShape = SeriesShape::FORWARD_STOCK_CENTS;

fn db() -> (tempfile::TempDir, SqliteTasks, Connection) {
    use std::os::unix::fs::PermissionsExt;
    // DatabaseLease requires a private, UID-owned, non-symlink runtime path —
    // same constraint compact_repo::tests::tempdir satisfies via /dev/shm.
    let dir = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/dev/shm")
        .unwrap();
    let tasks = SqliteTasks::new(dir.path().join("m.sqlite"));
    tasks.initialize_compact().unwrap();
    let c = tasks.connect(false).unwrap();
    (dir, tasks, c)
}

fn day(y: i32, m: u32, d: u32) -> i64 {
    compact_market::epoch_day(NaiveDate::from_ymd_opt(y, m, d).unwrap())
}

fn bar(day: i64, price: i64) -> CompactBar {
    CompactBar {
        trade_day: day,
        open_units: price,
        high_units: price + 1,
        low_units: price - 1,
        close_units: price,
        volume: Some(100),
        amount: Some("123.45".into()),
        r7_raw: Some(1),
    }
}

fn version_evidence() -> Value {
    serde_json::to_value(VersionEvidence {
        source: "test-source".into(),
        coverage_reason: "test coverage".into(),
        unit_currency: UnitCurrencyEvidence::confirmed(
            "CNY", "share", "share", "CNY", "yuan", "bound",
        ),
        adjustment_evidence: AdjustmentEvidence::confirmed("qfq", "bound"),
    })
    .unwrap()
}

fn put_test_evidence(c: &Connection) -> (Digest, Digest) {
    let version =
        super::super::evidence_store::put_evidence(c, EvidenceKind::Version, &version_evidence())
            .unwrap();
    let generation = super::super::evidence_store::put_evidence(
        c,
        EvidenceKind::Generation,
        &json!({"generation": "test"}),
    )
    .unwrap();
    (version, generation)
}

fn put_test_binding(c: &Connection, generation_evidence: Digest) -> Digest {
    super::super::evidence_store::put_source_binding(
        c,
        &SourceBinding {
            generation_id: "gen-test".into(),
            generation_manifest_sha256: [0x01; 32],
            day_member_sha256: [0x02; 32],
            security_event_sha256: [0x03; 32],
            evidence_digest: generation_evidence,
        },
    )
    .unwrap()
}

fn stub_item(c: &Connection, tag: &str) -> (String, String) {
    let task_id = format!("task-{tag}");
    let item_id = format!("item-{tag}");
    c.execute(
        "INSERT INTO tasks(task_id,command_key_json,status,state_revision,accepted_run_id,created_at,last_progress_at,frozen_work_json) VALUES(?1,'{}','pending',0,'run-1','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z','{}')",
        params![task_id],
    )
    .unwrap();
    c.execute(
        "INSERT INTO task_items(task_id,item_id,target_json,progress_unit,frozen_inputs_json,scope_keys_json,stage) VALUES(?1,?2,'{}','day','{}','{}','pending')",
        params![task_id, item_id],
    )
    .unwrap();
    (task_id, item_id)
}

fn event(
    entity: &str,
    period: Period,
    kind: &'static str,
    old: Option<Digest>,
    new: Digest,
    refs: (Digest, Digest),
    tag: &str,
) -> NewEvent {
    let (evidence, binding) = refs;
    NewEvent {
        entity: entity.into(),
        period,
        adjustment: Adjustment::Forward,
        kind,
        outcome: if kind == "no_change" { 1 } else { 2 },
        metadata_changed: kind == "metadata",
        continuity_mode: match kind {
            "append" => Some(2),
            "first_publish" | "replace" => Some(3),
            _ => None,
        },
        prefix_bytes: if kind == "append" { Some(0) } else { None },
        old_content_digest: old,
        new_content_digest: new,
        range_start_day: None,
        range_end_day: None,
        window_digest: None,
        base_content_digest: None,
        algorithm_version: None,
        source_binding_digest: Some(binding),
        evidence_digest: evidence,
        task_id: String::new(),
        item_id: String::new(),
        task_revision: 1,
        result_id: format!("result-{tag}"),
        result_digest: [0x0A; 32],
        created_at: "2026-01-01T00:00:00Z".into(),
        semantics_changed: 0,
    }
}

fn commit_event(c: &Connection, mut e: NewEvent, tag: &str) -> i64 {
    let (task_id, item_id) = stub_item(c, tag);
    e.task_id = task_id;
    e.item_id = item_id;
    let seq = next_event_seq(c).unwrap();
    insert_event(c, seq, &e).unwrap();
    seq
}

fn set_series_state(
    c: &Connection,
    series_key: i64,
    digest: Digest,
    record_count: i64,
    last_day: i64,
    last_event_seq: i64,
) {
    c.execute(
        "UPDATE market_series SET version_id=?1,content_digest=?2,record_count=?3,last_day=?4,last_event_seq=?5,updated_at='2026-01-02T00:00:00Z' WHERE series_key=?6",
        params![
            version_id(&digest),
            digest.as_slice(),
            record_count,
            last_day,
            last_event_seq,
            series_key
        ],
    )
    .unwrap();
}

fn seed_daily(
    c: &Connection,
    entity: &str,
    bars: &[CompactBar],
    evidence: Digest,
    binding: Digest,
) -> SeriesRow {
    seed_period(c, entity, Period::Day, bars, evidence, binding, "fp-d")
}

fn seed_period(
    c: &Connection,
    entity: &str,
    period: Period,
    bars: &[CompactBar],
    evidence: Digest,
    binding: Digest,
    tag: &str,
) -> SeriesRow {
    let digest = fold_bars(entity, period, SHAPE, bars).unwrap();
    let seq = commit_event(
        c,
        event(
            entity,
            period,
            "first_publish",
            None,
            digest,
            (evidence, binding),
            tag,
        ),
        tag,
    );
    let key = insert_series(
        c,
        &NewSeries {
            entity: entity.into(),
            period,
            adjustment: SHAPE.adjustment,
            instrument_class: SHAPE.instrument_class,
            price_scale: SHAPE.price_scale,
            content_digest: digest,
            record_count: bars.len() as i64,
            first_day: bars[0].trade_day,
            last_day: bars.last().unwrap().trade_day,
            coverage_status: CoverageStatus::Complete,
            source_binding_digest: binding,
            evidence_digest: evidence,
            last_event_seq: seq,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
        },
    )
    .unwrap();
    insert_bars(c, key, bars).unwrap();
    read_series(c, entity, period).unwrap().unwrap()
}

/// Commit an `append` event + bars + series-state update (§3.1-2).
fn apply_append(
    c: &Connection,
    entity: &str,
    new_bars: &[CompactBar],
    evidence: Digest,
    binding: Digest,
    tag: &str,
) -> SeriesRow {
    let s = read_series(c, entity, Period::Day).unwrap().unwrap();
    insert_bars(c, s.series_key, new_bars).unwrap();
    let digest = fold_more(s.content_digest, new_bars).unwrap();
    let seq = commit_event(
        c,
        event(
            entity,
            Period::Day,
            "append",
            Some(s.content_digest),
            digest,
            (evidence, binding),
            tag,
        ),
        tag,
    );
    set_series_state(
        c,
        s.series_key,
        digest,
        s.record_count + new_bars.len() as i64,
        new_bars.last().unwrap().trade_day,
        seq,
    );
    read_series(c, entity, Period::Day).unwrap().unwrap()
}

/// Commit a `replace` event + full bar swap + series-state update (§3.1-3).
fn apply_replace(
    c: &Connection,
    entity: &str,
    bars: &[CompactBar],
    evidence: Digest,
    binding: Digest,
    tag: &str,
) -> SeriesRow {
    let s = read_series(c, entity, Period::Day).unwrap().unwrap();
    c.execute(
        "DELETE FROM series_bars WHERE series_key=?1",
        params![s.series_key],
    )
    .unwrap();
    insert_bars(c, s.series_key, bars).unwrap();
    let digest = fold_bars(entity, Period::Day, SHAPE, bars).unwrap();
    let seq = commit_event(
        c,
        event(
            entity,
            Period::Day,
            "replace",
            Some(s.content_digest),
            digest,
            (evidence, binding),
            tag,
        ),
        tag,
    );
    set_series_state(
        c,
        s.series_key,
        digest,
        bars.len() as i64,
        bars.last().unwrap().trade_day,
        seq,
    );
    read_series(c, entity, Period::Day).unwrap().unwrap()
}

fn seed_yearly(
    c: &Connection,
    entity: &str,
    yearly_bars: &[CompactBar],
    evidence: Digest,
    binding: Digest,
    base: &SeriesRow,
    as_of: i64,
) -> SeriesRow {
    let yearly = seed_period(
        c,
        entity,
        Period::Year,
        yearly_bars,
        evidence,
        binding,
        "fp-y",
    );
    insert_lineage(
        c,
        &SeriesLineage {
            series_key: yearly.series_key,
            base_daily_digest: base.content_digest,
            base_event_seq: base.last_event_seq,
            base_adjustment: Adjustment::Forward,
            base_instrument_class: crate::market_data::INSTRUMENT_CLASS_STOCK,
            algorithm_version: "1".into(),
            first_trade_day: yearly_bars[0].trade_day,
            last_trade_day: yearly_bars.last().unwrap().trade_day,
            as_of_day: as_of,
        },
    )
    .unwrap();
    // The derivation event records the daily base it was computed from.
    c.execute(
        "UPDATE market_events SET base_content_digest=?1 WHERE entity=?2 AND period=2",
        params![base.content_digest.as_slice(), entity],
    )
    .unwrap();
    yearly
}

fn security() -> Security {
    Security {
        exchange: Exchange::Sh,
        code: "600006".into(),
    }
}

// §8.1-1: fold equivalence + path independence.
#[test]
fn digest_fold_is_path_independent() {
    let bars: Vec<_> = (0..5)
        .map(|i| bar(day(2025, 1, 2) + i, 1000 + i))
        .collect::<Vec<_>>();
    let whole = fold_bars(ENTITY, Period::Day, SHAPE, &bars).unwrap();
    let prefix = fold_bars(ENTITY, Period::Day, SHAPE, &bars[..3]).unwrap();
    assert_eq!(whole, fold_more(prefix, &bars[3..]).unwrap());
    // Seed binds identity: same bars, different entity/period → different digest.
    assert_ne!(
        whole,
        fold_bars("sh.600007", Period::Day, SHAPE, &bars).unwrap()
    );
    assert_ne!(
        whole,
        fold_bars(ENTITY, Period::Year, SHAPE, &bars).unwrap()
    );
    // Schema 22 §3.10: the seed also binds 品种 and 精度, so the same bar list
    // under a different series identity is a different version. (The
    // adjustment dimension gets its second value with the raw series, PR-2.)
    let index_shape = SeriesShape {
        instrument_class: crate::market_data::INSTRUMENT_CLASS_INDEX,
        ..SHAPE
    };
    assert_ne!(
        whole,
        fold_bars(ENTITY, Period::Day, index_shape, &bars).unwrap()
    );
    let six_dp_shape = SeriesShape {
        price_scale: 6,
        ..SHAPE
    };
    assert_ne!(
        whole,
        fold_bars(ENTITY, Period::Day, six_dp_shape, &bars).unwrap()
    );
    // Non-increasing input is rejected.
    let mut bad = bars.clone();
    bad.swap(0, 1);
    assert!(fold_bars(ENTITY, Period::Day, SHAPE, &bad).is_err());
}

// §8.1-2: version_id is hex(content_digest); the CHECK enforces the binding.
#[test]
fn version_id_is_content_derived_and_check_enforced() {
    let (_dir, _tasks, c) = db();
    let (ev, gen_ev) = put_test_evidence(&c);
    let binding = put_test_binding(&c, gen_ev);
    let s = seed_daily(
        &c,
        ENTITY,
        &[bar(day(2025, 1, 2), 1000), bar(day(2025, 1, 3), 1001)],
        ev,
        binding,
    );
    assert_eq!(s.version_id, version_id(&s.content_digest));
    assert_eq!(s.version_id.len(), 64);
    assert_eq!(
        s.version_id,
        crate::compact_market::digest_hex(&s.content_digest)
    );
    // A version_id/digest mismatch is rejected by the CHECK, not by code.
    let bad = c.execute(
        "INSERT INTO market_series(entity,period,adjustment,instrument_class,price_scale,version_id,content_digest,digest_format,record_count,first_day,last_day,coverage_status,source_binding_digest,evidence_digest,last_event_seq,created_at,updated_at) VALUES('sh.600007',1,1,0,2,'not-a-digest',?1,'series-fold-v2',1,?2,?2,1,?3,?4,1,'x','x')",
        params![
            vec![9u8; 32],
            day(2025, 1, 1),
            binding.as_slice(),
            ev.as_slice()
        ],
    );
    assert!(bad.is_err());
}

// Schema 22 DDL (issue-383 §3.2/§3.10): a v1 digest cannot be relabelled as
// v2, and yearly series exist only in the forward 口径.
#[test]
fn schema22_rejects_v1_digest_format_and_raw_yearly_series() {
    let (_dir, _tasks, c) = db();
    let (ev, gen_ev) = put_test_evidence(&c);
    let binding = put_test_binding(&c, gen_ev);
    let insert = |tag: u8, period: i64, adjustment: i64, digest_format: &str| {
        let digest = [tag; 32];
        c.execute(
            "INSERT INTO market_series(entity,period,adjustment,instrument_class,price_scale,version_id,content_digest,digest_format,record_count,first_day,last_day,coverage_status,source_binding_digest,evidence_digest,last_event_seq,created_at,updated_at) VALUES('sh.600008',?1,?2,0,2,?3,?4,?5,1,?6,?6,1,?7,?8,1,'x','x')",
            params![
                period,
                adjustment,
                version_id(&digest),
                digest.as_slice(),
                digest_format,
                day(2025, 1, 1),
                binding.as_slice(),
                ev.as_slice()
            ],
        )
    };
    assert!(
        insert(0x31, 1, 1, "series-fold-v1").is_err(),
        "a v1 digest format must not be relabelled as schema 22"
    );
    assert!(
        insert(0x32, 2, 0, "series-fold-v2").is_err(),
        "raw yearly series are not allowed"
    );
    assert!(insert(0x33, 1, 1, "series-fold-v2").is_ok());
}

// Schema 22 §3.9: `derived` joins the evidence vocabulary and round-trips
// through the content-addressed store (calendar/factor/index evidence lands in
// PR-4..PR-6; the kind itself is part of schema 22).
#[test]
fn derived_evidence_kind_round_trips() {
    let (_dir, _tasks, c) = db();
    let digest = crate::sqlite_market::evidence_store::put_evidence(
        &c,
        EvidenceKind::Derived,
        &json!({"note": "index volume unit is hand"}),
    )
    .unwrap();
    let (kind, canonical) =
        crate::sqlite_market::evidence_store::read_evidence(&c, &digest).unwrap();
    assert_eq!(kind, EvidenceKind::Derived);
    assert!(canonical.contains("index volume unit is hand"));
}

// issue-383 §17.1: the startup closure probe must scope by all three keys.
// A raw series of the same entity (a state PR-2 creates) closes against its
// own event chain, not the forward one.
#[test]
fn startup_check_scopes_closure_by_adjustment() {
    let (_dir, _tasks, c) = db();
    let (ev, gen_ev) = put_test_evidence(&c);
    let binding = put_test_binding(&c, gen_ev);
    let bars = [bar(day(2025, 1, 2), 1000), bar(day(2025, 1, 3), 1001)];
    seed_daily(&c, ENTITY, &bars, ev, binding);
    // Raw-口径 rows have no writer until PR-2; the schema and the closure
    // probe must already handle them, so this fixture inserts them directly.
    let (task_id, item_id) = stub_item(&c, "raw");
    let raw_digest = [0x5A; 32];
    let raw_seq = next_event_seq(&c).unwrap();
    c.execute(
        "INSERT INTO market_events(seq,entity,period,adjustment,kind,outcome,metadata_changed,new_content_digest,evidence_digest,task_id,item_id,task_revision,result_id,result_digest,created_at) VALUES(?1,?2,1,0,'first_publish',2,0,?3,?4,?5,?6,1,'result-raw',?7,'2026-01-01T00:00:00Z')",
        params![
            raw_seq,
            ENTITY,
            raw_digest.as_slice(),
            ev.as_slice(),
            task_id,
            item_id,
            [0x0Au8; 32].as_slice()
        ],
    )
    .unwrap();
    c.execute(
        "INSERT INTO market_series(entity,period,adjustment,instrument_class,price_scale,version_id,content_digest,digest_format,record_count,first_day,last_day,coverage_status,source_binding_digest,evidence_digest,last_event_seq,created_at,updated_at) VALUES(?1,1,0,0,2,?2,?3,'series-fold-v2',2,?4,?5,1,?6,?7,?8,'x','x')",
        params![
            ENTITY,
            version_id(&raw_digest),
            raw_digest.as_slice(),
            day(2025, 1, 2),
            day(2025, 1, 3),
            binding.as_slice(),
            ev.as_slice(),
            raw_seq
        ],
    )
    .unwrap();
    startup_check(&c).unwrap();
    // The forward head resolves by its own 口径 only.
    let forward = read_series(&c, ENTITY, Period::Day).unwrap().unwrap();
    assert_eq!(forward.adjustment, Adjustment::Forward);
    assert_ne!(forward.content_digest, raw_digest);
    // Dropping the raw event breaks only the raw series' closure.
    c.execute("DELETE FROM market_events WHERE adjustment=0", [])
        .unwrap();
    assert!(startup_check(&c).is_err());
}

// §8.1-3/4: closure invariants + the three tamper cases.
#[test]
fn startup_check_detects_broken_closure() {
    let (_dir, _tasks, c) = db();
    let (ev, gen_ev) = put_test_evidence(&c);
    let binding = put_test_binding(&c, gen_ev);
    let bars = [
        bar(day(2025, 1, 2), 1000),
        bar(day(2025, 1, 3), 1001),
        bar(day(2025, 1, 6), 1002),
    ];
    seed_daily(&c, ENTITY, &bars[..2], ev, binding);
    let appended = apply_append(&c, ENTITY, &bars[2..], ev, binding, "a1");
    startup_check(&c).unwrap();
    check_entity_chain(&c, ENTITY, Period::Day, Adjustment::Forward).unwrap();

    // (a) rewritten tail digest: fake a consistent (digest, id) pair — the
    // series CHECK still passes, but the event/series closure does not.
    let fake = [0xEE; 32];
    c.execute(
        "UPDATE market_series SET content_digest=?1,version_id=?2 WHERE series_key=?3",
        params![fake.as_slice(), version_id(&fake), appended.series_key],
    )
    .unwrap();
    assert!(startup_check(&c).is_err());
    c.execute(
        "UPDATE market_series SET content_digest=?1,version_id=?2 WHERE series_key=?3",
        params![
            appended.content_digest.as_slice(),
            appended.version_id,
            appended.series_key
        ],
    )
    .unwrap();
    startup_check(&c).unwrap();

    // (b) an event newer than the recorded tail (the series row was not
    // updated — e.g. a partial-commit anomaly in reverse).
    let seq = commit_event(
        &c,
        event(
            ENTITY,
            Period::Day,
            "metadata",
            Some(appended.content_digest),
            appended.content_digest,
            (ev, binding),
            "m1",
        ),
        "m1",
    );
    assert!(seq > appended.last_event_seq);
    assert!(startup_check(&c).is_err());
    c.execute("DELETE FROM market_events WHERE seq=?1", params![seq])
        .unwrap();
    startup_check(&c).unwrap();

    // (c) an event for an (entity, period) with no series row.
    commit_event(
        &c,
        event(
            "sh.600007",
            Period::Day,
            "first_publish",
            None,
            [0x07; 32],
            (ev, binding),
            "o1",
        ),
        "o1",
    );
    assert!(startup_check(&c).is_err());
    c.execute("DELETE FROM market_events WHERE entity='sh.600007'", [])
        .unwrap();
    startup_check(&c).unwrap();

    // (d) chain continuity is per-pair and checkable cheaply.
    let orphan_series = insert_series(
        &c,
        &NewSeries {
            entity: "sh.600009".into(),
            period: Period::Day,
            adjustment: SHAPE.adjustment,
            instrument_class: SHAPE.instrument_class,
            price_scale: SHAPE.price_scale,
            content_digest: [0x09; 32],
            record_count: 1,
            first_day: day(2025, 1, 2),
            last_day: day(2025, 1, 2),
            coverage_status: CoverageStatus::Complete,
            source_binding_digest: binding,
            evidence_digest: ev,
            last_event_seq: 999_999,
            created_at: "x".into(),
            updated_at: "x".into(),
        },
    )
    .unwrap();
    assert!(orphan_series > 0);
    assert!(startup_check(&c).is_err()); // last_event_seq names no event
}

// §8.1-4: first_publish replay — the committed event answers replay.
#[test]
fn first_publish_replay_anchor() {
    let (_dir, _tasks, c) = db();
    let (ev, gen_ev) = put_test_evidence(&c);
    let binding = put_test_binding(&c, gen_ev);
    seed_daily(&c, ENTITY, &[bar(day(2025, 1, 2), 1000)], ev, binding);
    let replay = event_by_result(&c, "result-fp-d").unwrap().unwrap();
    assert_eq!(replay.kind, "first_publish");
    // A replayed result_id cannot commit a second event.
    let mut dup = event(
        ENTITY,
        Period::Day,
        "append",
        None,
        [0x01; 32],
        (ev, binding),
        "dup",
    );
    dup.result_id = "result-fp-d".into();
    let (t, i) = stub_item(&c, "dup");
    dup.task_id = t;
    dup.item_id = i;
    let seq = next_event_seq(&c).unwrap();
    assert!(insert_event(&c, seq, &dup).is_err());
}

// §8.1-9: stale_version vs version_not_found.
#[test]
fn stale_pin_resolution() {
    let (_dir, _tasks, c) = db();
    let (ev, gen_ev) = put_test_evidence(&c);
    let binding = put_test_binding(&c, gen_ev);
    let v1 = seed_daily(&c, ENTITY, &[bar(day(2025, 1, 2), 1000)], ev, binding);
    let v2 = apply_append(&c, ENTITY, &[bar(day(2025, 1, 3), 1001)], ev, binding, "a1");
    assert!(matches!(
        require_series(&c, &v2.version_id),
        Ok(s) if s.series_key == v2.series_key
    ));
    assert!(matches!(
        require_series(&c, &v1.version_id),
        Err(Error::Stale(_))
    ));
    assert!(matches!(
        require_series(
            &c,
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        ),
        Err(Error::NotFound)
    ));
    assert!(matches!(
        require_series(&c, "version-1-synthetic"),
        Err(Error::NotFound)
    ));
}

// RULINGS A1: yearly stale classification.
#[test]
fn yearly_state_classification() {
    let (_dir, _tasks, c) = db();
    let (ev, gen_ev) = put_test_evidence(&c);
    let binding = put_test_binding(&c, gen_ev);
    let d1 = seed_daily(&c, ENTITY, &[bar(day(2025, 1, 2), 1000)], ev, binding);
    let y = seed_yearly(
        &c,
        ENTITY,
        &[bar(day(2025, 1, 2), 1000)],
        ev,
        binding,
        &d1,
        day(2025, 1, 2),
    );
    assert_eq!(yearly_state(&c, &y).unwrap(), YearlyState::Fresh);
    // append after the recorded base → behind (as_of preserved).
    let d2 = apply_append(&c, ENTITY, &[bar(day(2025, 1, 3), 1001)], ev, binding, "a1");
    assert_eq!(
        yearly_state(&c, &y).unwrap(),
        YearlyState::Behind {
            as_of_day: day(2025, 1, 2)
        }
    );
    // no_change / metadata events also keep it behind, not invalid.
    let seq = commit_event(
        &c,
        event(
            ENTITY,
            Period::Day,
            "no_change",
            Some(d2.content_digest),
            d2.content_digest,
            (ev, binding),
            "nc",
        ),
        "nc",
    );
    set_series_state(
        &c,
        d2.series_key,
        d2.content_digest,
        d2.record_count,
        d2.last_day,
        seq,
    );
    assert!(matches!(
        yearly_state(&c, &y).unwrap(),
        YearlyState::Behind { .. }
    ));
    // replace → invalid.
    apply_replace(&c, ENTITY, &[bar(day(2025, 2, 3), 2000)], ev, binding, "r1");
    assert_eq!(
        yearly_state(&c, &y).unwrap(),
        YearlyState::Invalid(YearlyInvalidReason::DailyReplaced)
    );
    // forged base digest → integrity error.
    let mut forged = y.clone();
    forged.series_key = -1;
    assert!(yearly_state(&c, &forged).is_err());
}

// Read paths through SqliteSupply on a real schema-21 file.
#[test]
fn supply_reads_on_schema21() {
    use std::os::unix::fs::PermissionsExt;
    let (_dir, tasks, c) = db();
    let (ev, gen_ev) = put_test_evidence(&c);
    let binding = put_test_binding(&c, gen_ev);
    let daily_bars = [
        bar(day(2025, 1, 2), 1000),
        bar(day(2025, 1, 3), 1001),
        bar(day(2025, 1, 6), 1002),
    ];
    let v1 = seed_daily(&c, ENTITY, &daily_bars[..2], ev, binding);
    // A correct yearly bar for the fixture: last trading day carries the
    // year's close, open is the first bar's, high/low the extremes.
    let yearly_bar = CompactBar {
        trade_day: day(2025, 1, 3),
        open_units: 1000,
        high_units: 1002,
        low_units: 999,
        close_units: 1001,
        volume: Some(200),
        amount: Some("246.9".into()),
        r7_raw: None,
    };
    seed_yearly(
        &c,
        ENTITY,
        std::slice::from_ref(&yearly_bar),
        ev,
        binding,
        &v1,
        day(2025, 1, 3),
    );
    // One committed append supersedes v1 → v1 becomes a `stale_version`.
    let d = apply_append(&c, ENTITY, &daily_bars[2..], ev, binding, "a2");
    drop(c);
    // DatabaseLease::acquire requires the database file to be private
    // (mode & 0o077 == 0) — same constraint compact_repo::tests::repo uses.
    std::fs::set_permissions(tasks.path(), std::fs::Permissions::from_mode(0o600)).unwrap();
    let backend = crate::sqlite_market::backend::MarketBackend::open(&tasks).unwrap();
    let supply = crate::sqlite_supply::SqliteSupply {
        market: backend.clone(),
    };
    let access = Access::local();
    let sec = security();

    // list_data_versions: exactly one current id per entity.
    let versions = supply
        .versions(
            &VersionsQuery {
                dataset: Dataset::Daily,
                contract_version: crate::data_supply::wire::ContractVersion::V1,
                selection: Selection::Single {
                    entity: sec.clone(),
                },
                page_size: 10,
                cursor: None,
            },
            &access,
        )
        .unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].version_ref.version_id, d.version_id);
    assert_eq!(versions[0].version_ref.integrity_sha256, d.version_id);

    // query_data latest: the three committed bars.
    let plan = QueryPlan::new(
        Query {
            dataset: Dataset::Daily,
            contract_version: crate::data_supply::wire::ContractVersion::V1,
            selection: Selection::Single {
                entity: sec.clone(),
            },
            filter: Filter::Daily {
                start_date: NaiveDate::from_ymd_opt(2025, 1, 1).unwrap(),
                end_date: NaiveDate::from_ymd_opt(2025, 12, 31).unwrap(),
            },
            version: VersionChoice::LatestPerEntity,
            page_size: 100,
            cursor: None,
        },
        &access,
    )
    .unwrap();
    let page = supply.query(&plan, &access).unwrap();
    assert_eq!(
        page.manifest[0].status,
        crate::data_supply::wire::ManifestStatus::Present
    );
    assert_eq!(page.rows.len(), 3);

    // Explicit pin on the current id resolves; a superseded id is Stale.
    let mut stale_args = plan.args.clone();
    stale_args.version = VersionChoice::ExplicitVersionMap {
        versions: vec![VersionBinding {
            entity: sec.clone(),
            version_id: d.version_id.clone(),
        }],
    };
    let ok = QueryPlan::new(stale_args.clone(), &access).unwrap();
    assert_eq!(supply.query(&ok, &access).unwrap().rows.len(), 3);
    // v1 was committed then superseded by the append → `stale_version`.
    stale_args.version = VersionChoice::ExplicitVersionMap {
        versions: vec![VersionBinding {
            entity: sec.clone(),
            version_id: v1.version_id.clone(),
        }],
    };
    let stale = QueryPlan::new(stale_args, &access).unwrap();
    assert!(matches!(
        supply.query(&stale, &access),
        Err(Error::Stale(_))
    ));

    // Yearly query resolves through the lineage and re-aggregated metadata.
    let yearly_plan = QueryPlan::new(
        Query {
            dataset: Dataset::Yearly,
            contract_version: crate::data_supply::wire::ContractVersion::V1,
            selection: Selection::Single {
                entity: sec.clone(),
            },
            filter: Filter::Yearly {
                start_year: 2025,
                end_year: 2025,
            },
            version: VersionChoice::LatestPerEntity,
            page_size: 100,
            cursor: None,
        },
        &access,
    )
    .unwrap();
    let year_page = supply.query(&yearly_plan, &access).unwrap();
    assert_eq!(
        year_page.manifest[0].status,
        crate::data_supply::wire::ManifestStatus::Present
    );
    assert_eq!(year_page.rows.len(), 1);
    match &year_page.rows[0] {
        crate::data_supply::wire::Row::Market {
            first_trading_date,
            last_trading_date,
            year_unfinished,
            ..
        } => {
            // as_of clips the re-aggregation to the derivation window — the
            // later daily append (Jan 6) does not leak into the stored row.
            assert_eq!(
                first_trading_date.unwrap(),
                NaiveDate::from_ymd_opt(2025, 1, 2).unwrap()
            );
            assert_eq!(
                last_trading_date.unwrap(),
                NaiveDate::from_ymd_opt(2025, 1, 3).unwrap()
            );
            assert!(year_unfinished.unwrap());
        }
        other => panic!("unexpected row: {other:?}"),
    }

    // availability: window facts straight off series_bars.
    let facts = supply.availability(&plan, &access).unwrap();
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].record_count, 3);

    // get_yearly_base_years: pinned current id resolves the year set.
    let presence = supply
        .year_presence(
            &crate::data_supply::presence::PresencePlan::new(
                YearPresenceQuery {
                    contract_version: crate::data_supply::wire::ContractVersion::V1,
                    items: vec![YearPresenceItem {
                        entity: sec.clone(),
                        base_daily_version_id: d.version_id.clone(),
                        start_year: 1,
                        end_year: 9999,
                    }],
                },
                &access,
            )
            .unwrap(),
            &access,
        )
        .unwrap();
    assert_eq!(presence.len(), 1);
    match &presence[0] {
        crate::data_supply::presence::PresenceOutcome::Resolved { years, .. } => {
            assert_eq!(years, &vec![2025]);
        }
        _ => panic!("unexpected presence outcome"),
    }
}

// R2' (OWNER-RULING-PAR-1A): get_yearly_base_years reason split — a pin
// committed then superseded is `base_superseded`, a never-committed id is
// `base_unavailable`, neither is integrity_failure.
#[test]
fn presence_reasons_distinguish_superseded_from_never_seen() {
    use std::os::unix::fs::PermissionsExt;
    let (_dir, tasks, c) = db();
    let (ev, gen_ev) = put_test_evidence(&c);
    let binding = put_test_binding(&c, gen_ev);
    let v1 = seed_daily(&c, ENTITY, &[bar(day(2025, 1, 2), 1000)], ev, binding);
    apply_append(
        &c,
        ENTITY,
        &[bar(day(2025, 1, 3), 1001)],
        ev,
        binding,
        "sup",
    );
    drop(c);
    std::fs::set_permissions(tasks.path(), std::fs::Permissions::from_mode(0o600)).unwrap();
    let backend = crate::sqlite_market::backend::MarketBackend::open(&tasks).unwrap();
    let supply = crate::sqlite_supply::SqliteSupply {
        market: backend.clone(),
    };
    let access = Access::local();
    // A well-formed digest that was never committed anywhere, on a second
    // entity — items are unique-entity, sorted by Security.
    let never_seen = "00".repeat(32);
    let other = Security {
        exchange: Exchange::Sh,
        code: "600007".into(),
    };
    let plan = crate::data_supply::presence::PresencePlan::new(
        YearPresenceQuery {
            contract_version: crate::data_supply::wire::ContractVersion::V1,
            items: vec![
                YearPresenceItem {
                    entity: security(),
                    base_daily_version_id: v1.version_id.clone(),
                    start_year: 1,
                    end_year: 9999,
                },
                YearPresenceItem {
                    entity: other,
                    base_daily_version_id: never_seen.clone(),
                    start_year: 1,
                    end_year: 9999,
                },
            ],
        },
        &access,
    )
    .unwrap();
    let outcomes = supply.year_presence(&plan, &access).unwrap();
    assert_eq!(outcomes.len(), 2);
    match &outcomes[0] {
        crate::data_supply::presence::PresenceOutcome::Unavailable(reason) => {
            assert_eq!(
                *reason,
                crate::data_supply::wire::YearPresenceReason::BaseSuperseded
            );
        }
        _ => panic!("superseded pin must be unavailable"),
    }
    match &outcomes[1] {
        crate::data_supply::presence::PresenceOutcome::Unavailable(reason) => {
            assert_eq!(
                *reason,
                crate::data_supply::wire::YearPresenceReason::BaseUnavailable
            );
        }
        _ => panic!("never-committed pin must be unavailable"),
    }
}

// §8.1-11: 1M-event startup timing fixture. Run manually:
// `cargo test startup_check_1m -- --ignored`. Seeded with foreign_keys=OFF so
// bulk events do not need stub task rows — the closure probes do not consult
// task_items, so timing is unaffected.
#[test]
#[ignore = "1M-event startup timing fixture; run manually, not on CI"]
fn startup_check_1m_events() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/dev/shm")
        .unwrap();
    let tasks = SqliteTasks::new(dir.path().join("m.sqlite"));
    tasks.initialize_compact().unwrap();
    let c = Connection::open(tasks.path()).unwrap();
    c.pragma_update(None, "foreign_keys", false).unwrap();
    c.pragma_update(None, "journal_mode", "wal").unwrap();
    let (ev, gen_ev) = put_test_evidence(&c);
    let binding = put_test_binding(&c, gen_ev);
    let entities = 5000usize;
    let events_each = 200usize;
    let tx = c.unchecked_transaction().unwrap();
    for e in 0..entities {
        let entity = format!("sh.{:06}", 600000 + e);
        let bars = [bar(day(2025, 1, 2), 1000)];
        let digest = fold_bars(&entity, Period::Day, SHAPE, &bars).unwrap();

        for n in 0..events_each {
            let kind = if n == 0 {
                "first_publish"
            } else if n % 3 == 0 {
                "no_change"
            } else {
                "metadata"
            };
            let mut ev_row = event(
                &entity,
                Period::Day,
                kind,
                if n == 0 { None } else { Some(digest) },
                digest,
                (ev, binding),
                "bulk",
            );
            ev_row.task_id = format!("t-{e}-{n}");
            ev_row.item_id = format!("i-{e}-{n}");
            ev_row.result_id = format!("r-{e}-{n}");
            let seq = next_event_seq(&tx).unwrap();
            insert_event(&tx, seq, &ev_row).unwrap();
            if n == events_each - 1 {
                insert_series(
                    &tx,
                    &NewSeries {
                        entity: entity.clone(),
                        period: Period::Day,
                        adjustment: SHAPE.adjustment,
                        instrument_class: SHAPE.instrument_class,
                        price_scale: SHAPE.price_scale,
                        content_digest: digest,
                        record_count: 1,
                        first_day: day(2025, 1, 2),
                        last_day: day(2025, 1, 2),
                        coverage_status: CoverageStatus::Complete,
                        source_binding_digest: binding,
                        evidence_digest: ev,
                        last_event_seq: seq,
                        created_at: "x".into(),
                        updated_at: "x".into(),
                    },
                )
                .unwrap();
            }
        }

        insert_bars(
            &tx,
            read_series(&tx, &entity, Period::Day)
                .unwrap()
                .unwrap()
                .series_key,
            &bars,
        )
        .unwrap();
    }
    tx.commit().unwrap();
    let started = std::time::Instant::now();
    startup_check(&c).unwrap();
    eprintln!("startup_check 1M events: {:?}", started.elapsed());
}

#[test]
fn deep_check_flags_forged_yearly_lineage() {
    // §3.5: a lineage row that names a base the daily series never had is
    // an integrity finding, not a silent classification.
    let (_dir, _tasks, c) = db();
    let (ev, gen_ev) = put_test_evidence(&c);
    let binding = put_test_binding(&c, gen_ev);
    let daily = seed_daily(&c, ENTITY, &[bar(day(2025, 1, 2), 1000)], ev, binding);
    let yearly = seed_yearly(
        &c,
        ENTITY,
        &[bar(day(2025, 12, 31), 1000)],
        ev,
        binding,
        &daily,
        day(2025, 12, 31),
    );
    assert!(deep_check(&c).unwrap().is_empty());

    // Forge the lineage base digest — the check must flag it.
    c.execute(
        "UPDATE yearly_series_lineage SET base_daily_digest=?1 WHERE series_key=?2",
        params![[0xEE_u8; 32].as_slice(), yearly.series_key],
    )
    .unwrap();
    let findings = deep_check(&c).unwrap();
    assert!(
        findings.iter().any(|f| f.contains("yearly lineage")),
        "forged lineage must surface in findings: {findings:?}"
    );
}
