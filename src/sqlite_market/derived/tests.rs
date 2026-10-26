//! M1 PR-4 (issue-383 §3.6/§3.7): the factor-event table, its whole-group
//! replace write path, the `adjustment-events-v1` digest and the version
//! registration for non-bar datasets.
//!
//! The `normalize(c_bits) == c1..c4` assertion deliberately reaches for the
//! source decoder's frozen rule (`gbbq::normalize_factor`): the point of the
//! test is that the stored derived decimals are exactly what that rule yields
//! from the stored immutable bits, so the factors can be recomputed from the
//! facts alone.

use super::*;
use crate::compact_market::EvidenceKind;
use crate::gbbq;
use crate::sqlite_tasks::SqliteTasks;
use rusqlite::{Connection, params};
use rust_decimal::Decimal;
use serde_json::{Value, json};

fn tempdir() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/dev/shm")
        .unwrap()
}

fn db() -> (tempfile::TempDir, SqliteTasks, Connection) {
    let dir = tempdir();
    let tasks = SqliteTasks::new(dir.path().join("m.sqlite"));
    tasks.initialize_compact().unwrap();
    let c = tasks.connect(false).unwrap();
    (dir, tasks, c)
}

/// A generation-bound source binding: the factor rows and the registration
/// both reference one, exactly as the series rows do.
fn seed_binding(c: &Connection) -> Digest {
    let generation = super::super::evidence_store::put_evidence(
        c,
        EvidenceKind::Generation,
        &json!({"generation": "gen-1"}),
    )
    .unwrap();
    super::super::evidence_store::put_source_binding(
        c,
        &crate::compact_market::SourceBinding {
            generation_id: "gen-1".into(),
            generation_manifest_sha256: [0x0b; 32],
            day_member_sha256: [0x0c; 32],
            security_event_sha256: [0x0e; 32],
            evidence_digest: generation,
        },
    )
    .unwrap()
}

fn day(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

/// One parsed fact: the f32 factors as the source stores them plus the 2dp
/// business decimals the frozen rule derives from those very bits.
fn event(ordinal: i64, effective_day: NaiveDate, c: [f32; 4]) -> AdjustmentEvent {
    AdjustmentEvent {
        ordinal,
        effective_day,
        category: 1,
        c_bits: c.map(f32::to_bits),
        factors: c.map(|value| gbbq::normalize_factor(value).unwrap()),
        known_at: None,
    }
}

fn set(entity: &str, events: Vec<AdjustmentEvent>, excluded: usize) -> AdjustmentEventSet {
    AdjustmentEventSet {
        entity: entity.into(),
        events,
        excluded,
    }
}

const ENTITY: &str = "sh.600006";

#[test]
fn events_digest_depends_on_values_and_order_only() {
    let a = event(0, day(2025, 7, 16), [0.0, 0.0, 0.0, 5.0]);
    let b = event(1, day(2026, 1, 9), [1.25, 0.0, 3.0, 0.0]);
    let digest = events_digest(ENTITY, &[a, b], 2).unwrap();
    // Same values, same order → same version, regardless of how the caller
    // built the set.
    assert_eq!(digest, events_digest(ENTITY, &[a, b], 2).unwrap());
    // Order is part of the identity (decode order, not a set).
    assert_ne!(digest, events_digest(ENTITY, &[b, a], 2).unwrap());
    // The excluded count never reaches the table but must still identify the
    // input set: a different ignored-record count is a different version.
    assert_ne!(digest, events_digest(ENTITY, &[a, b], 3).unwrap());
    // The immutable fact bits are folded; the entity is the seed.
    let mut moved = b;
    moved.c_bits[0] ^= 1;
    assert_ne!(digest, events_digest(ENTITY, &[a, moved], 2).unwrap());
    let mut renumbered = b;
    renumbered.ordinal = 7;
    assert_ne!(digest, events_digest(ENTITY, &[a, renumbered], 2).unwrap());
    assert_ne!(digest, events_digest("sz.000002", &[a, b], 2).unwrap());
    // The derived decimals are deliberately outside the digest: they are a
    // function of the bits, so folding both would let the digest disagree
    // with itself.
    let mut relabelled = b;
    relabelled.factors = [Decimal::ONE; 4];
    assert_eq!(digest, events_digest(ENTITY, &[a, relabelled], 2).unwrap());
}

#[test]
fn replace_stores_the_parsed_facts_and_registers_one_version() {
    let (_dir, _tasks, c) = db();
    let binding = seed_binding(&c);
    // Dividend (C1) and a 送转/配股 pair (C3/C4) — the two shapes the #54
    // contract covers.
    let events = vec![
        event(0, day(2025, 7, 16), [1.5, 0.0, 0.0, 0.0]),
        event(1, day(2026, 1, 9), [0.0, 3.2, 4.0, 2.0]),
    ];
    let set = set(ENTITY, events.clone(), 3);
    let written = replace_adjustment_events(&c, &set, binding, "task-1", "item-1")
        .unwrap()
        .expect("a non-empty set registers a version");

    // Identity: the version is the content digest in hex, and the digest
    // format / algorithm labels are the ones the design names.
    let digest = events_digest(ENTITY, &events, 3).unwrap();
    assert_eq!(written.version_id, compact_market::digest_hex(&digest));
    assert_eq!(written.content_digest, digest);
    assert_eq!(written.digest_format, "adjustment-events-v1");
    assert_eq!(written.algorithm_version, "gbbq-cat1-2dp-v1");
    assert_eq!(written.dataset_kind, "adjustment_events");
    assert_eq!(written.dataset_key, ENTITY);
    assert_eq!(written.record_count, 2);
    assert_eq!(
        written.first_day,
        compact_market::epoch_day(day(2025, 7, 16))
    );
    assert_eq!(written.last_day, compact_market::epoch_day(day(2026, 1, 9)));
    assert_eq!(written.source_binding_digest, binding);
    // Read back through the accessor the research read model will use.
    assert_eq!(
        read_version(&c, ADJUSTMENT_EVENTS_KIND, ENTITY).unwrap(),
        Some(written.clone())
    );

    // The parsed facts round-trip: bits verbatim, effective day, decode order.
    let stored = read_adjustment_events(&c, ENTITY).unwrap();
    assert_eq!(stored.len(), 2);
    for (read, original) in stored.iter().zip(&events) {
        assert_eq!(read.ordinal, original.ordinal);
        assert_eq!(read.effective_day, original.effective_day);
        assert_eq!(read.category, 1);
        assert_eq!(read.c_bits, original.c_bits);
        assert_eq!(read.factors, original.factors);
        // 分离点 3 / BR-M1-9: the source has no announcement date, so the
        // 知晓时间 is unknown — never inferred.
        assert_eq!(read.known_at, None);
    }
    // `normalize(c_bits) == c*`: the stored derived decimals are exactly the
    // frozen 2dp rule applied to the stored immutable bits, so the factor set
    // can be recomputed from the facts alone.
    for read in &stored {
        let expected = read
            .c_bits
            .map(|bits| gbbq::normalize_factor(f32::from_bits(bits)).unwrap());
        assert_eq!(read.factors, expected);
    }
    // Every row carries NULL known_at (not an empty string, not a proxy).
    let nulls: i64 = c
        .query_row(
            "SELECT count(*) FROM adjustment_events WHERE entity=?1 AND known_at IS NULL",
            params![ENTITY],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(nulls, 2);

    // The `derived` evidence bundle carries the excluded count and states the
    // known-at limitation, so the ignored records are accounted for even
    // though they are never stored.
    let (kind, canonical) =
        super::super::evidence_store::read_evidence(&c, &written.evidence_digest).unwrap();
    assert_eq!(kind, EvidenceKind::Derived);
    let evidence: Value = serde_json::from_str(&canonical).unwrap();
    assert_eq!(evidence["excluded_non_category_1"], 3);
    assert_eq!(evidence["event_count"], 2);
    assert_eq!(evidence["known_at"], "unknown");
    assert_eq!(evidence["dataset"], "adjustment_events");
    assert_eq!(evidence["dataset_key"], ENTITY);
    assert_eq!(evidence["algorithm_version"], "gbbq-cat1-2dp-v1");
    assert_eq!(evidence["content_digest"], written.version_id);
}

#[test]
fn replace_is_whole_group_and_an_empty_set_clears_the_registration() {
    let (_dir, _tasks, c) = db();
    let binding = seed_binding(&c);
    let first = set(
        ENTITY,
        vec![
            event(0, day(2025, 7, 16), [0.0, 0.0, 0.0, 5.0]),
            event(1, day(2025, 9, 1), [0.8, 0.0, 0.0, 0.0]),
            event(2, day(2026, 1, 9), [0.0, 0.0, 2.0, 0.0]),
        ],
        0,
    );
    let first_version = replace_adjustment_events(&c, &first, binding, "task-1", "item-1")
        .unwrap()
        .unwrap();

    // A corrected set replaces the whole group: the dropped ordinals do not
    // survive, and the version moves.
    let second = set(
        ENTITY,
        vec![event(0, day(2025, 7, 16), [0.0, 0.0, 0.0, 5.0])],
        1,
    );
    let second_version = replace_adjustment_events(&c, &second, binding, "task-2", "item-2")
        .unwrap()
        .unwrap();
    assert_ne!(first_version.version_id, second_version.version_id);
    assert_eq!(second_version.record_count, 1);
    let stored = read_adjustment_events(&c, ENTITY).unwrap();
    assert_eq!(stored.len(), 1, "stale ordinals must not survive");
    assert_eq!(stored[0].effective_day, day(2025, 7, 16));

    // An empty set is a real outcome: no rows and no registration, so the
    // table never claims a version it no longer holds.
    let cleared = replace_adjustment_events(
        &c,
        &AdjustmentEventSet::empty(ENTITY),
        binding,
        "task-3",
        "item-3",
    )
    .unwrap();
    assert!(cleared.is_none());
    assert!(read_adjustment_events(&c, ENTITY).unwrap().is_empty());
    assert_eq!(
        read_version(&c, ADJUSTMENT_EVENTS_KIND, ENTITY).unwrap(),
        None
    );
}

#[test]
fn replace_rejects_unverified_categories_unordered_ordinals_and_coarse_factors() {
    let (_dir, _tasks, c) = db();
    let binding = seed_binding(&c);
    let base = event(0, day(2025, 7, 16), [0.0, 0.0, 0.0, 5.0]);

    // Only the verified category is storable; anything else must have been
    // counted as excluded by the adapter, never handed over as a fact.
    let mut other_category = base;
    other_category.category = 2;
    assert!(
        replace_adjustment_events(&c, &set(ENTITY, vec![other_category], 0), binding, "t", "i")
            .is_err()
    );

    // Ordinals are the stable decode order: duplicates or a reordering cannot
    // be represented by the primary key, so the write fails closed.
    let unordered = set(
        ENTITY,
        vec![
            event(1, day(2025, 7, 16), [0.0, 0.0, 0.0, 5.0]),
            event(0, day(2025, 9, 1), [0.8, 0.0, 0.0, 0.0]),
        ],
        0,
    );
    assert!(replace_adjustment_events(&c, &unordered, binding, "t", "i").is_err());

    // The stored decimals are the 2dp business values; a coarser or finer
    // value means the derivation rule was not applied.
    let mut coarse = base;
    coarse.factors = [
        Decimal::new(12345, 4),
        Decimal::ZERO,
        Decimal::ZERO,
        Decimal::ZERO,
    ];
    assert!(
        replace_adjustment_events(&c, &set(ENTITY, vec![coarse], 0), binding, "t", "i").is_err()
    );

    // Nothing was written by any rejected call.
    assert!(read_adjustment_events(&c, ENTITY).unwrap().is_empty());
    assert_eq!(
        read_version(&c, ADJUSTMENT_EVENTS_KIND, ENTITY).unwrap(),
        None
    );
}

#[test]
fn an_entity_without_events_has_no_rows_and_no_registration() {
    let (_dir, _tasks, c) = db();
    assert!(read_adjustment_events(&c, "sz.000002").unwrap().is_empty());
    assert_eq!(
        read_version(&c, ADJUSTMENT_EVENTS_KIND, "sz.000002").unwrap(),
        None
    );
    // A non-canonical entity cannot own a factor set: the seed would not be a
    // stable identity.
    let binding = seed_binding(&c);
    assert!(
        replace_adjustment_events(
            &c,
            &set(
                "sh600006",
                vec![event(0, day(2025, 7, 16), [1.0, 0.0, 0.0, 0.0])],
                0
            ),
            binding,
            "t",
            "i",
        )
        .is_err()
    );
}

/// The DDL is the last line of defence: values the write path cannot produce
/// must be rejected by the table itself, not only by Rust-side checks.
#[test]
fn ddl_rejects_values_the_write_path_cannot_produce() {
    let (_dir, _tasks, c) = db();
    let binding = seed_binding(&c);
    let evidence = super::super::evidence_store::put_evidence(
        &c,
        EvidenceKind::Derived,
        &json!({"dataset": "adjustment_events"}),
    )
    .unwrap();
    let insert_event = |ordinal: i64, category: i64| {
        c.execute(
            "INSERT INTO adjustment_events(entity,ordinal,effective_day,category,\
             c_bits_1,c_bits_2,c_bits_3,c_bits_4,c1,c2,c3,c4,\
             source_binding_digest,evidence_digest,known_at) \
             VALUES(?1,?2,?3,?4,0,0,0,0,'0.00','0.00','0.00','0.00',?5,?6,NULL)",
            params![
                ENTITY,
                ordinal,
                20_000,
                category,
                binding.as_slice(),
                evidence.as_slice()
            ],
        )
    };
    insert_event(0, 1).unwrap();
    // Only the verified category is storable.
    assert!(insert_event(1, 2).is_err());
    // The primary key is the decode order: one row per ordinal.
    assert!(insert_event(0, 1).is_err());

    let digest = [7u8; 32];
    let insert_version = |version_id: &str, count: i64, first: i64, last: i64| {
        c.execute(
            "INSERT INTO derived_dataset_versions(dataset_kind,dataset_key,version_id,\
             content_digest,digest_format,record_count,first_day,last_day,algorithm_version,\
             source_binding_digest,evidence_digest,task_id,item_id,created_at,updated_at) \
             VALUES('trading_calendar','exchange_union',?1,?2,'trading-calendar-v1',?3,?4,?5,\
             'cal-union-v1',?6,?7,'t','i','now','now')",
            params![
                version_id,
                digest.as_slice(),
                count,
                first,
                last,
                binding.as_slice(),
                evidence.as_slice()
            ],
        )
    };
    // The registration's identity is the content digest, in hex.
    assert!(insert_version("not-a-digest", 1, 1, 1).is_err());
    // An empty dataset has no version.
    assert!(insert_version(&compact_market::digest_hex(&digest), 0, 1, 1).is_err());
    // The covered span must be a span.
    assert!(insert_version(&compact_market::digest_hex(&digest), 1, 2, 1).is_err());
    insert_version(&compact_market::digest_hex(&digest), 1, 1, 2).unwrap();
}
