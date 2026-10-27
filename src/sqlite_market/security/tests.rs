use std::collections::BTreeMap;

use rusqlite::Connection;
use serde_json::{Value, json};

use super::super::derived;
use super::*;
use crate::compact_market::EvidenceKind;
use crate::security_master::{
    CapitalEvent, DelistingReason, DelistingSource, Exchange, ExcludedCategories, IssuerResolution,
    ListingSource, SecurityLifecycleFact,
};
use crate::sqlite_tasks::SqliteTasks;

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

fn day(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
}

fn event(ordinal: i64, effective_day: NaiveDate, category: u8, c: [f32; 4]) -> CapitalEvent {
    CapitalEvent::from_raw(ordinal, effective_day, category, c.map(f32::to_bits)).unwrap()
}

fn set(
    entity: &str,
    events: Vec<CapitalEvent>,
    excluded: usize,
    excluded_by_category: ExcludedCategories,
) -> CapitalEventSet {
    CapitalEventSet {
        entity: entity.into(),
        events,
        excluded,
        excluded_by_category,
    }
}

fn fact(
    exchange: Exchange,
    code: &str,
    from: NaiveDate,
    to: Option<NaiveDate>,
) -> SecurityLifecycleFact {
    SecurityLifecycleFact {
        exchange,
        code: code.into(),
        effective_from: from,
        effective_to: to,
        instrument_class: 0,
        issuer_key: None,
        b_listing_date: None,
        listing_date: Some(from),
        listing_date_tdx: Some(from),
        listing_source: ListingSource::TdxSsdate,
        delisting_date: to,
        delisting_date_tdx: to,
        delisting_source: if to.is_some() {
            DelistingSource::TdxArrangement
        } else {
            DelistingSource::Unknown
        },
        delisting_reason_class: DelistingReason::Unknown,
        arrangement_start: to.map(|_| day(2013, 1, 4)),
        arrangement_end: to,
        issuer_resolution: IssuerResolution::CodeDate,
        listing_conflict: false,
        delisting_conflict: false,
    }
}

const ENTITY: &str = "sz.000001";

type MasterRow = (
    i64,
    i64,
    i64,
    String,
    i64,
    i64,
    String,
    i64,
    i64,
    String,
    String,
    i64,
    i64,
    i64,
    Option<String>,
);

#[test]
fn capital_digest_depends_on_facts_and_order_only() {
    let a = event(0, day(2024, 6, 20), 2, [0.0, 1000.0, 500.0, 1500.0]);
    let b = event(1, day(2025, 6, 20), 3, [0.0, 1000.0, 0.0, 1000.0]);
    let mut first = set(ENTITY, vec![a, b], 2, BTreeMap::from([(1, 1), (4, 1)]));
    let digest = capital_events_digest(&first).unwrap();
    assert_eq!(digest, capital_events_digest(&first).unwrap());
    // Order is part of the identity.
    let reordered = set(ENTITY, vec![b, a], 2, BTreeMap::from([(1, 1), (4, 1)]));
    assert_ne!(digest, capital_events_digest(&reordered).unwrap());
    // The excluded count is folded even though it never reaches the table.
    let mut more_excluded = first.clone();
    more_excluded.excluded = 3;
    assert_ne!(digest, capital_events_digest(&more_excluded).unwrap());
    // The immutable bits are folded.
    let mut moved = first.clone();
    moved.events[1].total_after_bits ^= 1;
    assert_ne!(digest, capital_events_digest(&moved).unwrap());
    // The derived share counts and the gate are not: they are recomputable.
    first.events[0].float_after += 1;
    first.events[0].suspicious = !first.events[0].suspicious;
    assert_eq!(digest, capital_events_digest(&first).unwrap());
}

#[test]
fn replace_stores_capital_facts_and_registers_one_version() {
    let (_dir, _tasks, c) = db();
    let binding = seed_binding(&c);
    let events = vec![
        event(0, day(2024, 6, 20), 2, [0.0, 1000.0, 500.0, 1500.0]),
        event(1, day(2025, 6, 20), 3, [0.0, 1000.0, 0.0, 1000.0]),
        event(2, day(2026, 6, 20), 9, [0.0, 1000.0, 1000.0, 1000.0]),
        event(3, day(2027, 6, 20), 10, [0.0, 1000.0, 1000.0, 1000.0]),
        // D-50 R-3 revision: cat5/7/8 are admitted. The before value stays
        // stored as audit evidence; only the after value is state.
        event(4, day(2028, 6, 20), 5, [1000.0, 1000.0, 600.0, 1500.0]),
        event(5, day(2029, 6, 20), 7, [1000.0, 1000.0, 700.0, 1500.0]),
        event(6, day(2030, 6, 20), 8, [1000.0, 1000.0, 800.0, 1500.0]),
    ];
    let set = set(ENTITY, events.clone(), 1, BTreeMap::from([(1, 1)]));
    let written = replace_capital_events(&c, &set, binding, "task-1", "item-1")
        .unwrap()
        .expect("a non-empty set registers a version");

    let digest = capital_events_digest(&set).unwrap();
    assert_eq!(written.version_id, compact_market::digest_hex(&digest));
    assert_eq!(written.content_digest, digest);
    assert_eq!(written.dataset_kind, "capital_events");
    assert_eq!(written.dataset_key, ENTITY);
    assert_eq!(written.digest_format, "capital-events-v1");
    assert_eq!(written.algorithm_version, "gbbq-capital-2dp-v1");
    assert_eq!(written.record_count, 7);
    assert_eq!(
        written.first_day,
        compact_market::epoch_day(day(2024, 6, 20))
    );
    assert_eq!(
        written.last_day,
        compact_market::epoch_day(day(2030, 6, 20))
    );
    assert_eq!(written.source_binding_digest, binding);

    let stored = read_capital_events(&c, ENTITY).unwrap();
    assert_eq!(stored.len(), 7);
    for (read, original) in stored.iter().zip(&events) {
        assert_eq!(read.ordinal, original.ordinal);
        assert_eq!(read.effective_day, original.effective_day);
        assert_eq!(read.category, original.category);
        assert_eq!(read.float_before_bits, original.float_before_bits);
        assert_eq!(read.total_before_bits, original.total_before_bits);
        assert_eq!(read.float_after_bits, original.float_after_bits);
        assert_eq!(read.total_after_bits, original.total_after_bits);
        assert_eq!(read.float_before, original.float_before);
        assert_eq!(read.total_before, original.total_before);
        assert_eq!(read.float_after, original.float_after);
        assert_eq!(read.total_after, original.total_after);
        assert!(read.prev_is_lagged);
        assert_eq!(read.suspicious, original.suspicious);
        assert_eq!(read.known_at, None);
    }
    // D-50: the admitted 5/7/8 rows store the after-value conversion, while
    // their before values remain available for audit.
    for (index, (category, float_after, total_after)) in [
        (5, 6_000_000, 15_000_000),
        (7, 7_000_000, 15_000_000),
        (8, 8_000_000, 15_000_000),
    ]
    .into_iter()
    .enumerate()
    {
        let row = &stored[4 + index];
        assert_eq!(
            (row.category, row.float_after, row.total_after),
            (category, float_after, total_after)
        );
        assert_eq!(
            (row.float_before, row.total_before),
            (10_000_000, 10_000_000)
        );
    }
    // `to_shares(bits)` is exactly the stored derived value.
    for read in &stored {
        assert_eq!(
            crate::security_master::to_shares(read.float_after_bits),
            Some(read.float_after)
        );
        assert_eq!(
            crate::security_master::to_shares(read.total_after_bits),
            Some(read.total_after)
        );
    }
    let nulls: i64 = c
        .query_row(
            "SELECT count(*) FROM capital_events WHERE exchange='sz' AND code='000001' \
             AND known_at IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(nulls, 7);

    let (kind, canonical) =
        super::super::evidence_store::read_evidence(&c, &written.evidence_digest).unwrap();
    assert_eq!(kind, EvidenceKind::Derived);
    let evidence: Value = serde_json::from_str(&canonical).unwrap();
    assert_eq!(evidence["dataset"], "capital_events");
    assert_eq!(evidence["dataset_key"], ENTITY);
    assert_eq!(evidence["event_count"], 7);
    assert_eq!(evidence["excluded_categories"], 1);
    assert_eq!(evidence["excluded_by_category"]["1"], 1);
    assert_eq!(evidence["excluded_by_category"].get("5"), None);
    assert_eq!(
        evidence["included_categories"],
        json!([2, 3, 5, 7, 8, 9, 10])
    );
    assert_eq!(evidence["known_at"], "unknown");
    assert_eq!(evidence["content_digest"], written.version_id);
    assert_eq!(evidence["suspicious_count"], 0);
    // Event 0 ends at 5,000,000 but event 1 starts at 0; event 2 ends at
    // 10,000,000 but event 3 starts at 0; event 4 ends at 6,000,000 but event
    // 5 starts at 10,000,000; event 5 ends at 7,000,000 but event 6 starts at
    // 10,000,000: §5.2 counts these four breaks.
    assert_eq!(evidence["chain_gap_count"], 4);
    assert_eq!(evidence["same_day_multi_event_count"], 0);
}

#[test]
fn replace_is_whole_group_and_an_empty_set_clears_the_registration() {
    let (_dir, _tasks, c) = db();
    let binding = seed_binding(&c);
    let first = set(
        ENTITY,
        vec![
            event(0, day(2024, 6, 20), 2, [0.0, 1000.0, 500.0, 1500.0]),
            event(1, day(2025, 6, 20), 3, [0.0, 1000.0, 0.0, 1000.0]),
            event(2, day(2026, 6, 20), 9, [0.0, 1000.0, 1000.0, 1000.0]),
        ],
        0,
        BTreeMap::new(),
    );
    let first_version = replace_capital_events(&c, &first, binding, "task-1", "item-1")
        .unwrap()
        .unwrap();

    let second = set(
        ENTITY,
        vec![event(0, day(2024, 6, 20), 2, [0.0, 1000.0, 500.0, 1500.0])],
        1,
        BTreeMap::from([(1, 1)]),
    );
    let second_version = replace_capital_events(&c, &second, binding, "task-2", "item-2")
        .unwrap()
        .unwrap();
    assert_ne!(first_version.version_id, second_version.version_id);
    assert_eq!(second_version.record_count, 1);
    let stored = read_capital_events(&c, ENTITY).unwrap();
    assert_eq!(stored.len(), 1, "stale ordinals must not survive");
    assert_eq!(stored[0].effective_day, day(2024, 6, 20));

    let cleared = replace_capital_events(
        &c,
        &CapitalEventSet::empty(ENTITY),
        binding,
        "task-3",
        "item-3",
    )
    .unwrap();
    assert!(cleared.is_none());
    assert!(read_capital_events(&c, ENTITY).unwrap().is_empty());
    assert_eq!(
        derived::read_version(&c, CAPITAL_EVENTS_KIND, ENTITY).unwrap(),
        None
    );
}

#[test]
fn replace_rejects_unadmitted_categories_inconsistent_shares_and_bad_order() {
    let (_dir, _tasks, c) = db();
    let binding = seed_binding(&c);
    let base = event(0, day(2024, 6, 20), 2, [0.0, 1000.0, 500.0, 1500.0]);

    let mut category_one = base;
    category_one.category = 1;
    assert!(
        replace_capital_events(
            &c,
            &set(ENTITY, vec![category_one], 0, BTreeMap::new()),
            binding,
            "t",
            "i"
        )
        .is_err()
    );

    let mut wrong_shares = base;
    wrong_shares.total_after += 1;
    assert!(
        replace_capital_events(
            &c,
            &set(ENTITY, vec![wrong_shares], 0, BTreeMap::new()),
            binding,
            "t",
            "i"
        )
        .is_err()
    );

    let mut wrong_gate = base;
    wrong_gate.suspicious = !wrong_gate.suspicious;
    assert!(
        replace_capital_events(
            &c,
            &set(ENTITY, vec![wrong_gate], 0, BTreeMap::new()),
            binding,
            "t",
            "i"
        )
        .is_err()
    );

    let mut not_lagged = base;
    not_lagged.prev_is_lagged = false;
    assert!(
        replace_capital_events(
            &c,
            &set(ENTITY, vec![not_lagged], 0, BTreeMap::new()),
            binding,
            "t",
            "i"
        )
        .is_err()
    );

    let out_of_order = set(
        ENTITY,
        vec![base, event(0, day(2025, 6, 20), 3, [0.0, 0.0, 0.0, 0.0])],
        0,
        BTreeMap::new(),
    );
    assert!(replace_capital_events(&c, &out_of_order, binding, "t", "i").is_err());
}

#[test]
fn the_suspicious_gate_is_stored_not_dropped() {
    let (_dir, _tasks, c) = db();
    let binding = seed_binding(&c);
    let jump = event(0, day(2024, 6, 20), 2, [0.0, 1000.0, 100.0, 1600.0]);
    let float_drop = event(1, day(2025, 6, 20), 3, [1000.0, 1600.0, 900.0, 1600.0]);
    let normal = event(2, day(2026, 6, 20), 9, [1000.0, 1600.0, 1000.0, 1600.0]);
    assert!(jump.suspicious);
    assert!(float_drop.suspicious);
    assert!(!normal.suspicious);
    let set = set(ENTITY, vec![jump, float_drop, normal], 0, BTreeMap::new());
    let written = replace_capital_events(&c, &set, binding, "t", "i")
        .unwrap()
        .unwrap();
    let stored = read_capital_events(&c, ENTITY).unwrap();
    assert_eq!(
        stored
            .iter()
            .map(|event| event.suspicious)
            .collect::<Vec<_>>(),
        vec![true, true, false]
    );
    let (_, canonical) =
        super::super::evidence_store::read_evidence(&c, &written.evidence_digest).unwrap();
    let evidence: Value = serde_json::from_str(&canonical).unwrap();
    assert_eq!(evidence["suspicious_count"], 2);
}

#[test]
fn master_upsert_writes_tdx_only_rows_and_keeps_other_rows() {
    let (_dir, _tasks, c) = db();
    let binding = seed_binding(&c);
    let rows = vec![
        fact(
            Exchange::Sh,
            "600001",
            day(1991, 11, 1),
            Some(day(2013, 12, 31)),
        ),
        fact(Exchange::Sz, "000001", day(1991, 4, 3), None),
    ];
    assert_eq!(
        upsert_security_master(&c, &rows, binding, "task-1", "item-1").unwrap(),
        2
    );

    let stored: MasterRow = c
        .query_row(
            "SELECT effective_from,effective_to,instrument_class,issuer_resolution,\
             listing_date,listing_date_tdx,listing_source,delisting_date,delisting_date_tdx,\
             delisting_source,delisting_reason_class,arrangement_start,arrangement_end,\
             listing_conflict,known_at FROM security_master \
             WHERE exchange='sh' AND code='600001'",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                    r.get(10)?,
                    r.get(11)?,
                    r.get(12)?,
                    r.get(13)?,
                    r.get(14)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(stored.0, compact_market::epoch_day(day(1991, 11, 1)));
    assert_eq!(stored.1, compact_market::epoch_day(day(2013, 12, 31)));
    assert_eq!(stored.2, 0);
    assert_eq!(stored.3, "code_date");
    assert_eq!(stored.4, compact_market::epoch_day(day(1991, 11, 1)));
    assert_eq!(stored.5, compact_market::epoch_day(day(1991, 11, 1)));
    assert_eq!(stored.6, "tdx_ssdate");
    assert_eq!(stored.7, compact_market::epoch_day(day(2013, 12, 31)));
    assert_eq!(stored.8, compact_market::epoch_day(day(2013, 12, 31)));
    assert_eq!(stored.9, "tdx_arrangement");
    assert_eq!(stored.10, "unknown");
    assert_eq!(stored.11, compact_market::epoch_day(day(2013, 1, 4)));
    assert_eq!(stored.12, compact_market::epoch_day(day(2013, 12, 31)));
    assert_eq!(stored.13, 0);
    assert_eq!(stored.14, None);

    // No `security_master` version registration: PR-3 owns the assembled
    // dataset (§9.3).
    let versions: i64 = c
        .query_row(
            "SELECT count(*) FROM derived_dataset_versions WHERE dataset_kind='security_master'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(versions, 0);

    let evidence: Value = {
        let digest: Vec<u8> = c
            .query_row(
                "SELECT evidence_digest FROM security_master WHERE exchange='sz' AND code='000001'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let digest: Digest = digest.as_slice().try_into().unwrap();
        let (kind, canonical) = super::super::evidence_store::read_evidence(&c, &digest).unwrap();
        assert_eq!(kind, EvidenceKind::Derived);
        serde_json::from_str(&canonical).unwrap()
    };
    assert_eq!(evidence["dataset"], "security_master");
    assert_eq!(evidence["dataset_key"], "tdx_only");
    assert_eq!(evidence["row_count"], 2);
    assert_eq!(evidence["known_at"], "unknown");
    assert_eq!(evidence["limitations"].as_array().unwrap().len(), 5);

    // A later batch that does not mention a code must not delete its row.
    let only_one = vec![fact(
        Exchange::Sh,
        "600001",
        day(1991, 11, 1),
        Some(day(2013, 12, 30)),
    )];
    assert_eq!(
        upsert_security_master(&c, &only_one, binding, "task-2", "item-2").unwrap(),
        1
    );
    let total: i64 = c
        .query_row("SELECT count(*) FROM security_master", [], |r| r.get(0))
        .unwrap();
    assert_eq!(total, 2);
    let updated: i64 = c
        .query_row(
            "SELECT effective_to FROM security_master WHERE exchange='sh' AND code='600001'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(updated, compact_market::epoch_day(day(2013, 12, 30)));
}

#[test]
fn master_rejects_inconsistent_sources_and_duplicate_intervals() {
    let (_dir, _tasks, c) = db();
    let binding = seed_binding(&c);
    let mut unknown_listing = fact(Exchange::Sz, "000001", day(1991, 4, 3), None);
    unknown_listing.listing_source = ListingSource::Unknown;
    assert!(upsert_security_master(&c, &[unknown_listing], binding, "t", "i").is_err());

    let mut date_without_source = fact(Exchange::Sz, "000001", day(1991, 4, 3), None);
    date_without_source.delisting_date = Some(day(2013, 12, 31));
    date_without_source.delisting_source = DelistingSource::Unknown;
    assert!(upsert_security_master(&c, &[date_without_source], binding, "t", "i").is_err());

    let mut bad_interval = fact(Exchange::Sz, "000001", day(1991, 4, 3), None);
    bad_interval.effective_to = Some(day(1991, 4, 3));
    assert!(upsert_security_master(&c, &[bad_interval], binding, "t", "i").is_err());

    let duplicate = fact(Exchange::Sz, "000001", day(1991, 4, 3), None);
    assert!(
        upsert_security_master(&c, &[duplicate.clone(), duplicate], binding, "t", "i").is_err()
    );
}

#[test]
fn replace_master_writes_the_assembled_dataset_and_registers_a_version() {
    use crate::security_master::assembly::{
        self, Assembly, CodeMigrationFact, Conflict, MigrationCertainty, PendingSource,
    };

    let (_dir, _tasks, c) = db();
    let binding = seed_binding(&c);
    let earlier = SecurityLifecycleFact {
        exchange: Exchange::Sh,
        code: "600018".into(),
        effective_from: day(2000, 7, 19),
        effective_to: Some(day(2006, 10, 26)),
        instrument_class: 0,
        issuer_key: None,
        issuer_resolution: IssuerResolution::CodeDate,
        listing_date: Some(day(2000, 7, 19)),
        listing_date_tdx: None,
        listing_source: ListingSource::ExchangeList,
        b_listing_date: None,
        delisting_date: None,
        delisting_date_tdx: None,
        delisting_source: DelistingSource::Unknown,
        delisting_reason_class: DelistingReason::Unknown,
        arrangement_start: None,
        arrangement_end: None,
        listing_conflict: true,
        delisting_conflict: false,
    };
    let later = SecurityLifecycleFact {
        exchange: Exchange::Sh,
        code: "600018".into(),
        effective_from: day(2006, 10, 26),
        effective_to: None,
        instrument_class: 0,
        issuer_key: Some("上港集团".into()),
        issuer_resolution: IssuerResolution::Ambiguous,
        listing_date: Some(day(2006, 10, 26)),
        listing_date_tdx: Some(day(2006, 10, 26)),
        listing_source: ListingSource::TdxSsdate,
        b_listing_date: None,
        delisting_date: None,
        delisting_date_tdx: None,
        delisting_source: DelistingSource::Unknown,
        delisting_reason_class: DelistingReason::Unknown,
        arrangement_start: None,
        arrangement_end: None,
        listing_conflict: true,
        delisting_conflict: false,
    };
    let migration = CodeMigrationFact {
        from_exchange: Exchange::Sz,
        from_code: "300114".into(),
        to_exchange: Exchange::Sz,
        to_code: "302132".into(),
        effective_from: day(2025, 2, 17),
        certainty: MigrationCertainty::Confirmed,
        source_kind: "exchange_announcement".into(),
        source_record_id: Some("ann-1".into()),
        known_at: Some(day(2025, 2, 15)),
    };
    let assembled = Assembly {
        rows: vec![earlier.clone(), later.clone()],
        migrations: vec![migration.clone()],
        conflicts: vec![Conflict::ListingDate {
            exchange: Exchange::Sh,
            code: "600018".into(),
            exchange_date: day(2000, 7, 19),
            tdx_date: day(2006, 10, 26),
        }],
        pending_sources: vec![PendingSource {
            dataset: assembly::DATASET_KIND,
            source: "exchange_list",
            reason: "test",
        }],
        unresolved: vec![],
        exchange_as_of: Some(day(2026, 10, 26)),
    };
    let written = replace_security_master(&c, &assembled, binding, "task-1", "item-1").unwrap();
    assert_eq!(written.dataset_kind, assembly::DATASET_KIND);
    assert_eq!(written.dataset_key, assembly::DATASET_KEY);
    assert_eq!(written.digest_format, assembly::DIGEST_FORMAT);
    assert_eq!(written.algorithm_version, assembly::ALGORITHM_VERSION);
    assert_eq!(written.record_count, 2);
    assert_eq!(
        written.content_digest,
        assembly::assembly_digest(&assembled.rows, &assembled.migrations)
    );
    assert_eq!(
        written.version_id,
        compact_market::digest_hex(&written.content_digest)
    );
    assert_eq!(
        written.first_day,
        compact_market::epoch_day(day(2000, 7, 19))
    );
    assert_eq!(
        written.last_day,
        compact_market::epoch_day(day(2006, 10, 26))
    );
    assert_eq!(written.source_binding_digest, binding);

    assert_eq!(
        read_security_master(&c).unwrap(),
        vec![earlier.clone(), later.clone()]
    );
    assert_eq!(read_code_migrations(&c).unwrap(), vec![migration]);

    let (kind, canonical) =
        super::super::evidence_store::read_evidence(&c, &written.evidence_digest).unwrap();
    assert_eq!(kind, EvidenceKind::Derived);
    let evidence: Value = serde_json::from_str(&canonical).unwrap();
    assert_eq!(evidence["dataset"], "security_master");
    assert_eq!(evidence["dataset_key"], "a_share");
    assert_eq!(evidence["row_count"], 2);
    assert_eq!(evidence["migration_count"], 1);
    assert_eq!(evidence["confirmed_migration_count"], 1);
    assert_eq!(evidence["candidate_migration_count"], 0);
    assert_eq!(evidence["listing_conflict_count"], 2);
    assert_eq!(evidence["ambiguous_code_reuse_count"], 1);
    assert_eq!(evidence["conflicts"][0]["kind"], "listing_date");
    assert_eq!(evidence["pending_sources"][0]["source"], "exchange_list");
    assert_eq!(evidence["known_at"], "unknown");
    assert_eq!(evidence["exchange_as_of"], "2026-10-26");
    assert_eq!(evidence["limitations"].as_array().unwrap().len(), 4);

    // The second generation replaces the dataset: the old interval and the
    // migration must not survive as stale rows.
    let one = Assembly {
        rows: vec![later.clone()],
        migrations: vec![],
        conflicts: vec![],
        pending_sources: vec![],
        unresolved: vec![],
        exchange_as_of: Some(day(2026, 10, 26)),
    };
    let second = replace_security_master(&c, &one, binding, "task-2", "item-2").unwrap();
    assert_ne!(written.version_id, second.version_id);
    assert_eq!(second.record_count, 1);
    assert_eq!(read_security_master(&c).unwrap(), vec![later]);
    assert!(read_code_migrations(&c).unwrap().is_empty());
}

#[test]
fn replace_master_rejects_an_empty_assembly() {
    use crate::security_master::assembly::Assembly;

    let (_dir, _tasks, c) = db();
    let binding = seed_binding(&c);
    let empty = Assembly {
        rows: vec![],
        migrations: vec![],
        conflicts: vec![],
        pending_sources: vec![],
        unresolved: vec![],
        exchange_as_of: None,
    };
    assert!(replace_security_master(&c, &empty, binding, "t", "i").is_err());
    assert!(read_security_master(&c).unwrap().is_empty());
}
