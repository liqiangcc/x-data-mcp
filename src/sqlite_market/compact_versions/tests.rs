use rust_decimal::Decimal;

use super::*;

fn connection() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch(
        "CREATE TABLE evidence_bundles(
           evidence_digest BLOB PRIMARY KEY CHECK(length(evidence_digest)=32),
           evidence_kind TEXT NOT NULL CHECK(evidence_kind IN ('generation','batch_selection','version','continuity','catalog','yearly')),
           canonical_json TEXT NOT NULL
         ) STRICT, WITHOUT ROWID;
         CREATE TABLE evidence_parts(
           part_digest BLOB PRIMARY KEY CHECK(length(part_digest)=32),
           canonical_json TEXT NOT NULL
         ) STRICT, WITHOUT ROWID;
         CREATE TABLE source_bindings(
           source_binding_digest BLOB PRIMARY KEY CHECK(length(source_binding_digest)=32),
           generation_id TEXT NOT NULL,
           generation_manifest_sha256 BLOB NOT NULL CHECK(length(generation_manifest_sha256)=32),
           day_member_sha256 BLOB NOT NULL CHECK(length(day_member_sha256)=32),
           security_event_sha256 BLOB NOT NULL CHECK(length(security_event_sha256)=32),
           evidence_digest BLOB NOT NULL REFERENCES evidence_bundles(evidence_digest)
         ) STRICT, WITHOUT ROWID;
         CREATE TABLE bar_segments(
           segment_key INTEGER PRIMARY KEY,
           segment_digest BLOB NOT NULL UNIQUE CHECK(length(segment_digest)=32),
           entity TEXT NOT NULL,
           period INTEGER NOT NULL CHECK(period IN (1,2)),
           first_day INTEGER NOT NULL,
           last_day INTEGER NOT NULL,
           row_count INTEGER NOT NULL CHECK(row_count BETWEEN 1 AND 512),
           CHECK(first_day <= last_day)
         ) STRICT;
         CREATE TABLE segment_bars(
           segment_key INTEGER NOT NULL REFERENCES bar_segments(segment_key),
           trade_day INTEGER NOT NULL,
           open_cents INTEGER NOT NULL,
           high_cents INTEGER NOT NULL,
           low_cents INTEGER NOT NULL,
           close_cents INTEGER NOT NULL,
           volume INTEGER,
           amount TEXT,
           r7_raw INTEGER,
           PRIMARY KEY(segment_key,trade_day),
           CHECK(high_cents >= open_cents AND high_cents >= close_cents AND high_cents >= low_cents),
           CHECK(low_cents <= open_cents AND low_cents <= close_cents),
           CHECK(volume IS NULL OR volume >= 0),
           CHECK(r7_raw IS NULL OR r7_raw BETWEEN 0 AND 4294967295)
         ) STRICT, WITHOUT ROWID;
         CREATE TABLE data_versions(
           version_key INTEGER PRIMARY KEY,
           version_id TEXT NOT NULL UNIQUE,
           entity TEXT NOT NULL,
           period INTEGER NOT NULL CHECK(period IN (1,2)),
           adjustment INTEGER NOT NULL CHECK(adjustment=1),
           state INTEGER NOT NULL CHECK(state IN (1,2,3)),
           source_binding_digest BLOB NOT NULL REFERENCES source_bindings(source_binding_digest),
           parent_version_key INTEGER REFERENCES data_versions(version_key),
           manifest_mode INTEGER NOT NULL CHECK(manifest_mode IN (1,2)),
           delta_depth INTEGER NOT NULL CHECK(delta_depth BETWEEN 0 AND 256),
           record_count INTEGER NOT NULL CHECK(record_count > 0),
           first_day INTEGER NOT NULL,
           last_day INTEGER NOT NULL,
           coverage_status INTEGER NOT NULL CHECK(coverage_status IN (1,2,3)),
           evidence_digest BLOB NOT NULL REFERENCES evidence_bundles(evidence_digest),
           projection_digest BLOB NOT NULL CHECK(length(projection_digest)=32),
           integrity_digest BLOB CHECK(integrity_digest IS NULL OR length(integrity_digest)=32),
           digest_format TEXT NOT NULL CHECK(digest_format='compact-market-version-v1'),
           created_at TEXT NOT NULL,
           published_at TEXT,
           CHECK(first_day <= last_day),
           CHECK((manifest_mode=1 AND parent_version_key IS NULL AND delta_depth=0)
              OR (manifest_mode=2 AND parent_version_key IS NOT NULL AND delta_depth BETWEEN 1 AND 256)),
           CHECK((state IN (1,2) AND published_at IS NULL AND integrity_digest IS NULL)
              OR (state=3 AND published_at IS NOT NULL AND integrity_digest IS NOT NULL))
         ) STRICT;
         CREATE INDEX data_versions_entity_period_published ON data_versions(entity, period, adjustment, published_at DESC, version_key DESC);
         CREATE TABLE version_checkpoint_clips(
           version_key INTEGER NOT NULL REFERENCES data_versions(version_key),
           seq INTEGER NOT NULL CHECK(seq >= 0),
           segment_key INTEGER NOT NULL REFERENCES bar_segments(segment_key),
           clip_start_day INTEGER NOT NULL,
           clip_end_day INTEGER NOT NULL,
           row_count INTEGER NOT NULL CHECK(row_count > 0),
           PRIMARY KEY(version_key,seq),
           CHECK(clip_start_day <= clip_end_day)
         ) STRICT, WITHOUT ROWID;
         CREATE TABLE version_delta_clips(
           version_key INTEGER NOT NULL REFERENCES data_versions(version_key),
           seq INTEGER NOT NULL CHECK(seq >= 0),
           delta_kind INTEGER NOT NULL CHECK(delta_kind IN (1,2)),
           range_start_day INTEGER NOT NULL,
           range_end_day INTEGER NOT NULL,
           segment_key INTEGER REFERENCES bar_segments(segment_key),
           clip_start_day INTEGER,
           clip_end_day INTEGER,
           row_count INTEGER NOT NULL CHECK(row_count >= 0),
           PRIMARY KEY(version_key,seq),
           CHECK(range_start_day <= range_end_day),
           CHECK((delta_kind=1 AND segment_key IS NOT NULL AND clip_start_day IS NOT NULL
                  AND clip_end_day IS NOT NULL AND row_count > 0)
              OR (delta_kind=2 AND segment_key IS NULL AND clip_start_day IS NULL
                  AND clip_end_day IS NULL AND row_count=0))
         ) STRICT, WITHOUT ROWID;
         CREATE TABLE yearly_lineage(
           version_key INTEGER PRIMARY KEY REFERENCES data_versions(version_key),
           base_daily_version_key INTEGER NOT NULL REFERENCES data_versions(version_key),
           algorithm_version TEXT NOT NULL,
           first_trade_day INTEGER NOT NULL,
           last_trade_day INTEGER NOT NULL,
           as_of_day INTEGER NOT NULL,
           CHECK(first_trade_day <= last_trade_day AND last_trade_day <= as_of_day)
         ) STRICT;
         CREATE TABLE effective_heads(
           entity TEXT NOT NULL,
           period INTEGER NOT NULL CHECK(period IN (1,2)),
           adjustment INTEGER NOT NULL CHECK(adjustment=1),
           version_key INTEGER NOT NULL UNIQUE REFERENCES data_versions(version_key),
           PRIMARY KEY(entity,period,adjustment)
         ) STRICT;",
    )
    .unwrap();
    c
}

fn lax(c: &Connection) {
    c.execute_batch("PRAGMA foreign_keys=OFF; PRAGMA ignore_check_constraints=ON")
        .unwrap();
    let foreign_keys: i64 = c
        .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
        .unwrap();
    assert_eq!(foreign_keys, 0);
}

fn cb(day: i64, cents: i64) -> CompactBar {
    CompactBar {
        trade_day: day,
        open_cents: cents,
        high_cents: cents + 10,
        low_cents: cents - 10,
        close_cents: cents,
        volume: None,
        amount: None,
        r7_raw: None,
    }
}

fn cb_full(day: i64, cents: i64, volume: i64, amount: &str) -> CompactBar {
    CompactBar {
        trade_day: day,
        open_cents: cents,
        high_cents: cents + 10,
        low_cents: cents - 10,
        close_cents: cents,
        volume: Some(volume),
        amount: Some(amount.into()),
        r7_raw: Some(7),
    }
}

fn segment(c: &Connection, entity: &str, period: Period, bars: &[CompactBar]) -> StoredSegment {
    compact_v19::insert_segment(c, entity, period, bars).unwrap()
}

fn clip(segment: &StoredSegment, start: i64, end: i64, count: u32) -> StoredClip {
    StoredClip {
        segment_key: segment.key,
        clip: SegmentClip {
            segment_digest: segment.digest,
            clip_start_day: start,
            clip_end_day: end,
            row_count: count,
        },
    }
}

fn full_clip(segment: &StoredSegment) -> StoredClip {
    clip(
        segment,
        segment.bars.first().unwrap().trade_day,
        segment.bars.last().unwrap().trade_day,
        segment.bars.len() as u32,
    )
}

fn upsert(segment: &StoredSegment, range_start: i64, range_end: i64) -> StoredDelta {
    StoredDelta {
        range_start_day: range_start,
        range_end_day: range_end,
        kind: StoredDeltaKind::Upsert(clip(
            segment,
            range_start,
            range_end,
            segment
                .bars
                .iter()
                .filter(|bar| bar.trade_day >= range_start && bar.trade_day <= range_end)
                .count() as u32,
        )),
    }
}

fn remove(start: i64, end: i64) -> StoredDelta {
    StoredDelta {
        range_start_day: start,
        range_end_day: end,
        kind: StoredDeltaKind::Remove,
    }
}

fn unit(status: &str, value: Value) -> Value {
    serde_json::json!({"status": status, "value": value, "reason": "bound"})
}

fn version_evidence(source: &str, volume_known: bool, amount_known: bool) -> Value {
    serde_json::json!({
        "source": source,
        "coverage_reason": "full history verified",
        "unit_currency": {
            "price_currency": unit("Confirmed", serde_json::json!("CNY")),
            "price_unit": unit("Confirmed", serde_json::json!("share")),
            "volume_unit": if volume_known { unit("Confirmed", serde_json::json!("share")) } else { unit("Unknown", Value::Null) },
            "amount_currency": if amount_known { unit("Confirmed", serde_json::json!("CNY")) } else { unit("Unknown", Value::Null) },
            "amount_unit": if amount_known { unit("Confirmed", serde_json::json!("yuan")) } else { unit("Unknown", Value::Null) }
        },
        "adjustment_evidence": unit("Confirmed", serde_json::json!("qfq"))
    })
}

fn put_version_evidence(c: &Connection) -> Digest {
    put_evidence(
        c,
        EvidenceKind::Version,
        &version_evidence("official", true, true),
    )
    .unwrap()
}

fn put_binding(c: &Connection) -> Digest {
    let evidence = put_evidence(
        c,
        EvidenceKind::Generation,
        &serde_json::json!({"generation": "gen-1"}),
    )
    .unwrap();
    put_source_binding(
        c,
        &SourceBinding {
            generation_id: "gen-1".into(),
            generation_manifest_sha256: [1; 32],
            day_member_sha256: [2; 32],
            security_event_sha256: [3; 32],
            evidence_digest: evidence,
        },
    )
    .unwrap()
}

fn mk_draft(version_id: &str, entity: &str, period: Period, c: &Connection) -> VersionDraft {
    VersionDraft {
        version_id: version_id.into(),
        entity: entity.into(),
        period,
        source_binding_digest: put_binding(c),
        coverage_status: CoverageStatus::Complete,
        evidence_digest: put_version_evidence(c),
        created_at: "2026-09-21T00:00:00+00:00".into(),
        yearly_lineage: None,
    }
}

fn ts() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-09-21T00:00:00+00:00")
        .unwrap()
        .with_timezone(&Utc)
}

fn publish_daily(c: &Connection, version_id: &str, bars: &[CompactBar]) -> (i64, Digest) {
    let seg = segment(c, "sh.600006", Period::Day, bars);
    let draft = mk_draft(version_id, "sh.600006", Period::Day, c);
    let key = stage_checkpoint(c, &draft, &[full_clip(&seg)], None).unwrap();
    mark_validated(c, key).unwrap();
    let digest = publish(c, key, ts()).unwrap();
    (key, digest)
}

fn integrity_failure<T>(result: Result<T, Error>) {
    match result {
        Err(Error::Integrity(_)) => {}
        Err(e) => panic!("expected Integrity failure, got {e}"),
        Ok(_) => panic!("expected Integrity failure, got Ok"),
    }
}

fn mk_scope(entity: &str, period: Period) -> Scope {
    Scope {
        symbol: entity.into(),
        period,
    }
}

#[test]
fn evidence_and_source_binding_lifecycle() {
    let c = connection();
    let value = serde_json::json!({"b": 1, "a": 2});
    let digest = put_evidence(&c, EvidenceKind::Continuity, &value).unwrap();
    assert_eq!(
        put_evidence(&c, EvidenceKind::Continuity, &value).unwrap(),
        digest
    );
    let count: i64 = c
        .query_row("SELECT count(*) FROM evidence_bundles", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);

    c.execute(
        "UPDATE evidence_bundles SET canonical_json='{\"b\":1,\"a\":2}' WHERE evidence_digest=?1",
        params![digest.as_slice()],
    )
    .unwrap();
    integrity_failure(put_evidence(&c, EvidenceKind::Continuity, &value));
    c.execute(
        "UPDATE evidence_bundles SET canonical_json='{\"a\":2,\"b\":1}',evidence_kind='catalog' WHERE evidence_digest=?1",
        params![digest.as_slice()],
    )
    .unwrap();
    integrity_failure(put_evidence(&c, EvidenceKind::Continuity, &value));

    let version_evidence = put_version_evidence(&c);
    let binding = SourceBinding {
        generation_id: "gen-1".into(),
        generation_manifest_sha256: [1; 32],
        day_member_sha256: [2; 32],
        security_event_sha256: [3; 32],
        evidence_digest: version_evidence,
    };
    integrity_failure(put_source_binding(&c, &binding));
    let mut missing = binding.clone();
    missing.evidence_digest = [0xee; 32];
    integrity_failure(put_source_binding(&c, &missing));

    let generation = put_evidence(
        &c,
        EvidenceKind::Generation,
        &serde_json::json!({"generation": "gen-1"}),
    )
    .unwrap();
    let binding = SourceBinding {
        evidence_digest: generation,
        ..binding
    };
    let binding_digest = put_source_binding(&c, &binding).unwrap();
    assert_eq!(put_source_binding(&c, &binding).unwrap(), binding_digest);
    c.execute(
        "UPDATE source_bindings SET generation_id='gen-2' WHERE source_binding_digest=?1",
        params![binding_digest.as_slice()],
    )
    .unwrap();
    integrity_failure(put_source_binding(&c, &binding));
}

#[test]
fn checkpoint_daily_lifecycle() {
    let c = connection();
    let bars = vec![cb_full(0, 100, 10, "100"), cb_full(1, 110, 20, "200")];
    let seg = segment(&c, "sh.600006", Period::Day, &bars);
    let version_draft = mk_draft("v1", "sh.600006", Period::Day, &c);
    let key = stage_checkpoint(&c, &version_draft, &[full_clip(&seg)], None).unwrap();
    let (state, mode, parent, depth): (i64, i64, Option<i64>, i64) = c
        .query_row(
            "SELECT state,manifest_mode,parent_version_key,delta_depth FROM data_versions WHERE version_key=?1",
            params![key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!((state, mode, parent, depth), (1, 1, None, 0));
    assert!(matches!(
        stage_checkpoint(&c, &version_draft, &[full_clip(&seg)], None),
        Err(Error::Conflict(_))
    ));
    integrity_failure(publish(&c, key, ts()));
    mark_validated(&c, key).unwrap();
    integrity_failure(mark_validated(&c, key));
    let digest = publish(&c, key, ts()).unwrap();
    integrity_failure(mark_validated(&c, key));
    integrity_failure(publish(&c, key, ts()));

    let identity = VersionIdentity {
        version_id: "v1".into(),
        entity: "sh.600006".into(),
        period: Period::Day,
        source_binding_digest: version_draft.source_binding_digest,
        parent_version_id: None,
        manifest_mode: ManifestMode::Checkpoint,
        delta_depth: 0,
        projection_digest: compact_market::checkpoint_projection_digest(
            "sh.600006",
            Period::Day,
            &[seg_clip(&seg)],
        )
        .unwrap(),
        record_count: 2,
        first_day: 0,
        last_day: 1,
        coverage_status: CoverageStatus::Complete,
        evidence_digest: version_draft.evidence_digest,
        published_at: "2026-09-21T00:00:00+00:00".into(),
        yearly_lineage: None,
    };
    let expected = compact_market::version_integrity_digest(&identity).unwrap();
    assert_eq!(digest, expected);

    let verified = fixed(&c, "v1").unwrap();
    assert_eq!(verified.key, key);
    assert_eq!(verified.integrity_digest, digest);
    assert_eq!(verified.version.version_id, "v1");
    assert_eq!(verified.version.symbol, "sh.600006");
    assert_eq!(verified.version.period, Period::Day);
    assert_eq!(verified.version.adjustment, Adjustment::Forward);
    assert_eq!(verified.version.source, "official");
    assert_eq!(verified.version.published_at, ts());
    assert_eq!(verified.version.records.len(), 2);
    assert_eq!(
        verified.version.records[0],
        bars[0].to_bar(Period::Day).unwrap()
    );
    assert_eq!(verified.version.validation.record_count, 2);
    assert_eq!(verified.version.validation.duplicate_count, 0);
    assert!(verified.version.yearly_derivation.is_none());
    assert!(matches!(fixed(&c, "absent"), Err(Error::NotFound)));
}

fn seg_clip(seg: &StoredSegment) -> SegmentClip {
    full_clip(seg).clip
}

#[test]
fn range_reads() {
    let c = connection();
    let bars: Vec<CompactBar> = (0..10).map(|d| cb(d, 100)).collect();
    let (key, _) = publish_daily(&c, "v1", &bars);
    let scope = mk_scope("sh.600006", Period::Day);
    let all = Range {
        start: NaiveDate::from_ymd_opt(1970, 1, 1).unwrap(),
        end: NaiveDate::from_ymd_opt(1970, 12, 31).unwrap(),
        after: None,
        limit: 100,
    };
    let page = range(&c, &scope, Some("v1"), &all).unwrap();
    assert_eq!(page.rows.len(), 10);
    assert_eq!(page.next_after, None);
    assert_eq!(page.version.version_id, "v1");

    c.execute(
        "INSERT INTO effective_heads(entity,period,adjustment,version_key) VALUES('sh.600006',1,1,?1)",
        params![key],
    )
    .unwrap();
    let via_head = range(&c, &scope, None, &all).unwrap();
    assert_eq!(via_head.rows.len(), 10);

    let window = Range {
        start: NaiveDate::from_ymd_opt(1970, 1, 3).unwrap(),
        end: NaiveDate::from_ymd_opt(1970, 1, 5).unwrap(),
        after: None,
        limit: 100,
    };
    let page = range(&c, &scope, Some("v1"), &window).unwrap();
    assert_eq!(page.rows.len(), 3);
    assert_eq!(
        page.rows[0].date,
        NaiveDate::from_ymd_opt(1970, 1, 3).unwrap()
    );

    let paged = Range {
        after: Some(NaiveDate::from_ymd_opt(1970, 1, 3).unwrap()),
        limit: 3,
        ..window.clone()
    };
    let page = range(&c, &scope, Some("v1"), &paged).unwrap();
    assert_eq!(page.rows.len(), 2);
    assert_eq!(
        page.rows[0].date,
        NaiveDate::from_ymd_opt(1970, 1, 4).unwrap()
    );

    let limited = Range {
        limit: 4,
        ..all.clone()
    };
    let page = range(&c, &scope, Some("v1"), &limited).unwrap();
    assert_eq!(page.rows.len(), 4);
    assert_eq!(
        page.next_after,
        Some(NaiveDate::from_ymd_opt(1970, 1, 4).unwrap())
    );

    assert!(matches!(
        range(&c, &scope, Some("absent"), &all),
        Err(Error::NotFound)
    ));
    assert!(matches!(
        range(&c, &mk_scope("sz.000002", Period::Day), None, &all),
        Err(Error::NotFound)
    ));
    integrity_failure(range(
        &c,
        &mk_scope("sz.000002", Period::Day),
        Some("v1"),
        &all,
    ));
}

#[test]
fn delta_semantics() {
    let c = connection();
    let (base_key, _) = publish_daily(&c, "v1", &[cb(0, 100), cb(1, 110), cb(2, 120)]);
    let revision = segment(&c, "sh.600006", Period::Day, &[cb(1, 115), cb(3, 130)]);
    let draft = mk_draft("v2", "sh.600006", Period::Day, &c);
    let key = stage_delta(&c, &draft, base_key, &[upsert(&revision, 1, 3)]).unwrap();
    mark_validated(&c, key).unwrap();
    publish(&c, key, ts()).unwrap();
    let verified = fixed(&c, "v2").unwrap();
    assert_eq!(verified.version.records.len(), 4);
    assert_eq!(
        verified
            .version
            .records
            .iter()
            .map(|bar| compact_market::epoch_day(bar.date))
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );
    assert_eq!(verified.version.records[1].close, Decimal::new(115, 2));
    let base = fixed(&c, "v1").unwrap();
    assert_eq!(base.version.records.len(), 3);
    assert_eq!(base.version.records[1].close, Decimal::new(110, 2));

    let draft = mk_draft("v3", "sh.600006", Period::Day, &c);
    let key = stage_delta(&c, &draft, key, &[remove(1, 2)]).unwrap();
    mark_validated(&c, key).unwrap();
    publish(&c, key, ts()).unwrap();
    let verified = fixed(&c, "v3").unwrap();
    assert_eq!(
        verified
            .version
            .records
            .iter()
            .map(|bar| compact_market::epoch_day(bar.date))
            .collect::<Vec<_>>(),
        vec![0, 3]
    );

    let draft = mk_draft("v4", "sh.600006", Period::Day, &c);
    integrity_failure(stage_delta(&c, &draft, key, &[]));
}

#[test]
fn stage_delta_rejects_corrupted_parent() {
    let c = connection();
    let (base_key, _) = publish_daily(&c, "v1", &[cb(0, 100), cb(1, 110)]);
    c.execute(
        "UPDATE data_versions SET integrity_digest=?1 WHERE version_key=?2",
        params![[0xabu8; 32].as_slice(), base_key],
    )
    .unwrap();
    let extra = segment(&c, "sh.600006", Period::Day, &[cb(2, 120)]);
    let draft = mk_draft("v2", "sh.600006", Period::Day, &c);
    integrity_failure(stage_delta(&c, &draft, base_key, &[upsert(&extra, 2, 2)]));
    let exists: bool = c
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM data_versions WHERE version_id='v2')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!exists);
}

#[test]
fn list_rejects_corrupted_parent_linkage() {
    let c = connection();
    let (base_key, _) = publish_daily(&c, "v1", &[cb(0, 100)]);
    let extra = segment(&c, "sh.600006", Period::Day, &[cb(1, 110)]);
    let draft = mk_draft("v2", "sh.600006", Period::Day, &c);
    let key = stage_delta(&c, &draft, base_key, &[upsert(&extra, 1, 1)]).unwrap();
    mark_validated(&c, key).unwrap();
    publish(&c, key, ts()).unwrap();
    assert_eq!(
        list(&c, &mk_scope("sh.600006", Period::Day)).unwrap().len(),
        2
    );
    lax(&c);
    c.execute(
        "UPDATE data_versions SET state=2 WHERE version_key=?1",
        params![base_key],
    )
    .unwrap();
    integrity_failure(list(&c, &mk_scope("sh.600006", Period::Day)));
    c.execute(
        "UPDATE data_versions SET state=3,entity='sz.000002' WHERE version_key=?1",
        params![base_key],
    )
    .unwrap();
    integrity_failure(list(&c, &mk_scope("sh.600006", Period::Day)));
}

#[test]
fn chain_corruptions_fail_closed() {
    let c = connection();
    let (base_key, _) = publish_daily(&c, "v1", &[cb(0, 100), cb(1, 110)]);
    let extra = segment(&c, "sh.600006", Period::Day, &[cb(2, 120)]);
    let draft = mk_draft("v2", "sh.600006", Period::Day, &c);
    let delta_key = stage_delta(&c, &draft, base_key, &[upsert(&extra, 2, 2)]).unwrap();
    mark_validated(&c, delta_key).unwrap();
    publish(&c, delta_key, ts()).unwrap();

    lax(&c);
    c.execute(
        "UPDATE data_versions SET parent_version_key=9999 WHERE version_key=?1",
        params![delta_key],
    )
    .unwrap();
    integrity_failure(fixed(&c, "v2"));
    c.execute(
        "UPDATE data_versions SET parent_version_key=?1 WHERE version_key=?1",
        params![delta_key],
    )
    .unwrap();
    integrity_failure(fixed(&c, "v2"));
    c.execute(
        "UPDATE data_versions SET parent_version_key=?1,delta_depth=5 WHERE version_key=?2",
        params![base_key, delta_key],
    )
    .unwrap();
    integrity_failure(fixed(&c, "v2"));
    c.execute(
        "UPDATE data_versions SET delta_depth=300 WHERE version_key=?1",
        params![delta_key],
    )
    .unwrap();
    integrity_failure(fixed(&c, "v2"));
    c.execute(
        "UPDATE data_versions SET delta_depth=1,manifest_mode=9 WHERE version_key=?1",
        params![delta_key],
    )
    .unwrap();
    integrity_failure(fixed(&c, "v2"));
    c.execute(
        "UPDATE data_versions SET manifest_mode=1 WHERE version_key=?1",
        params![delta_key],
    )
    .unwrap();
    integrity_failure(fixed(&c, "v2"));
    c.execute(
        "UPDATE data_versions SET manifest_mode=2 WHERE version_key=?1",
        params![delta_key],
    )
    .unwrap();
    c.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    c.execute(
        "DELETE FROM version_delta_clips WHERE version_key=?1",
        params![delta_key],
    )
    .unwrap();
    integrity_failure(fixed(&c, "v2"));
}

#[test]
fn metadata_and_payload_corruptions_fail_closed() {
    let c = connection();
    let (base_key, _) = publish_daily(&c, "v1", &[cb(0, 100), cb(1, 110)]);
    let (good_projection, good_integrity): (Vec<u8>, Vec<u8>) = c
        .query_row(
            "SELECT projection_digest,integrity_digest FROM data_versions WHERE version_key=?1",
            params![base_key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    c.execute(
        "UPDATE data_versions SET projection_digest=?1 WHERE version_key=?2",
        params![[0xabu8; 32].as_slice(), base_key],
    )
    .unwrap();
    integrity_failure(fixed(&c, "v1"));
    c.execute(
        "UPDATE data_versions SET projection_digest=?1 WHERE version_key=?2",
        params![good_projection, base_key],
    )
    .unwrap();
    c.execute(
        "UPDATE data_versions SET integrity_digest=?1 WHERE version_key=?2",
        params![[0xabu8; 32].as_slice(), base_key],
    )
    .unwrap();
    integrity_failure(fixed(&c, "v1"));
    c.execute(
        "UPDATE data_versions SET integrity_digest=?1,record_count=99 WHERE version_key=?2",
        params![good_integrity, base_key],
    )
    .unwrap();
    integrity_failure(fixed(&c, "v1"));
    c.execute(
        "UPDATE data_versions SET record_count=2 WHERE version_key=?1",
        params![base_key],
    )
    .unwrap();
    c.execute(
        "UPDATE segment_bars SET close_cents=close_cents+1 WHERE trade_day=0",
        [],
    )
    .unwrap();
    integrity_failure(fixed(&c, "v1"));
    c.execute(
        "DELETE FROM version_checkpoint_clips WHERE version_key=?1",
        params![base_key],
    )
    .unwrap();
    integrity_failure(fixed(&c, "v1"));
}

#[test]
fn cross_entity_parent_rejected() {
    let c = connection();
    let (base_key, _) = publish_daily(&c, "v1", &[cb(0, 100)]);
    let seg_b = segment(&c, "sz.000002", Period::Day, &[cb(0, 200)]);
    let draft_b = mk_draft("w1", "sz.000002", Period::Day, &c);
    let key_b = stage_checkpoint(&c, &draft_b, &[full_clip(&seg_b)], None).unwrap();
    mark_validated(&c, key_b).unwrap();
    publish(&c, key_b, ts()).unwrap();

    let extra = segment(&c, "sh.600006", Period::Day, &[cb(1, 110)]);
    let draft = mk_draft("v2", "sh.600006", Period::Day, &c);
    integrity_failure(stage_delta(&c, &draft, key_b, &[upsert(&extra, 1, 1)]));

    let draft = mk_draft("v3", "sh.600006", Period::Day, &c);
    let delta_key = stage_delta(&c, &draft, base_key, &[upsert(&extra, 1, 1)]).unwrap();
    mark_validated(&c, delta_key).unwrap();
    publish(&c, delta_key, ts()).unwrap();
    lax(&c);
    c.execute(
        "UPDATE data_versions SET parent_version_key=?1 WHERE version_key=?2",
        params![key_b, delta_key],
    )
    .unwrap();
    integrity_failure(fixed(&c, "v3"));
}

#[test]
fn checkpoint_basis_requires_depth_and_equality() {
    let c = connection();
    let (mut parent, _) = publish_daily(&c, "v0", &[cb(0, 100)]);
    for i in 1..=256i64 {
        let seg = segment(&c, "sh.600006", Period::Day, &[cb(i, 100 + i)]);
        let draft = mk_draft(&format!("v{i}"), "sh.600006", Period::Day, &c);
        let key = stage_delta(&c, &draft, parent, &[upsert(&seg, i, i)]).unwrap();
        mark_validated(&c, key).unwrap();
        publish(&c, key, ts()).unwrap();
        parent = key;
    }
    let next = segment(&c, "sh.600006", Period::Day, &[cb(257, 357)]);
    let mut clips = Vec::new();
    let mut statement = c
        .prepare("SELECT segment_key FROM bar_segments ORDER BY segment_key")
        .unwrap();
    let keys = statement
        .query_map([], |r| r.get::<_, i64>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    drop(statement);
    for key in keys {
        clips.push(full_clip(&compact_v19::read_segment(&c, key).unwrap()));
    }
    let basis = CheckpointBasis {
        parent_version_key: parent,
        delta_ops: vec![upsert(&next, 257, 257)],
    };
    let draft = mk_draft("v257", "sh.600006", Period::Day, &c);
    let key = stage_checkpoint(&c, &draft, &clips, Some(&basis)).unwrap();
    let count: i64 = c
        .query_row(
            "SELECT record_count FROM data_versions WHERE version_key=?1",
            params![key],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 258);

    let mut short = clips.clone();
    short.pop();
    let draft = mk_draft("v258", "sh.600006", Period::Day, &c);
    integrity_failure(stage_checkpoint(&c, &draft, &short, Some(&basis)));

    let shallow = CheckpointBasis {
        parent_version_key: clips_parent(&c, "v0"),
        delta_ops: vec![upsert(&next, 257, 257)],
    };
    let draft = mk_draft("v259", "sh.600006", Period::Day, &c);
    integrity_failure(stage_checkpoint(&c, &draft, &clips, Some(&shallow)));

    let empty = CheckpointBasis {
        parent_version_key: parent,
        delta_ops: vec![],
    };
    let draft = mk_draft("v260", "sh.600006", Period::Day, &c);
    integrity_failure(stage_checkpoint(&c, &draft, &short, Some(&empty)));

    c.execute(
        "UPDATE data_versions SET integrity_digest=?1 WHERE version_key=?2",
        params![[0xabu8; 32].as_slice(), parent],
    )
    .unwrap();
    let draft = mk_draft("v261", "sh.600006", Period::Day, &c);
    integrity_failure(stage_checkpoint(&c, &draft, &clips, Some(&basis)));
    let exists: bool = c
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM data_versions WHERE version_id='v261')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!exists);
}

/// Chain of `depth` delta appends, then a checkpoint with a one-op basis on
/// the head. Returns the staging outcome.
fn rollover_over_parent_depth(depth: i64) -> Result<i64, Error> {
    let c = connection();
    let (mut parent, _) = publish_daily(&c, "v0", &[cb(0, 100)]);
    for i in 1..=depth {
        let seg = segment(&c, "sh.600006", Period::Day, &[cb(i, 100 + i)]);
        let draft = mk_draft(&format!("v{i}"), "sh.600006", Period::Day, &c);
        let key = stage_delta(&c, &draft, parent, &[upsert(&seg, i, i)]).unwrap();
        mark_validated(&c, key).unwrap();
        publish(&c, key, ts()).unwrap();
        parent = key;
    }
    let next_day = depth + 1;
    let next = segment(
        &c,
        "sh.600006",
        Period::Day,
        &[cb(next_day, 100 + next_day)],
    );
    let mut statement = c
        .prepare("SELECT segment_key FROM bar_segments ORDER BY segment_key")
        .unwrap();
    let keys = statement
        .query_map([], |r| r.get::<_, i64>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    drop(statement);
    let mut clips = Vec::new();
    for key in keys {
        clips.push(full_clip(&compact_v19::read_segment(&c, key).unwrap()));
    }
    // `next` is the newest segment; it belongs to the basis op and the
    // checkpoint alike (clips cover every stored segment).
    let basis = CheckpointBasis {
        parent_version_key: parent,
        delta_ops: vec![upsert(&next, next_day, next_day)],
    };
    let draft = mk_draft("v-roll", "sh.600006", Period::Day, &c);
    stage_checkpoint(&c, &draft, &clips, Some(&basis))
}

/// XP-051: a rollover checkpoint is accepted over any parent depth in
/// ROLLOVER_FLOOR..=256 and rejected below the floor.
#[test]
fn checkpoint_basis_accepts_parent_depth_from_the_floor() {
    integrity_failure(rollover_over_parent_depth(i64::from(ROLLOVER_FLOOR) - 1));
    for depth in [i64::from(ROLLOVER_FLOOR), 100, 127, 256] {
        rollover_over_parent_depth(depth)
            .unwrap_or_else(|e| panic!("parent depth {depth} rejected: {e}"));
    }
}

/// XP-051: the per-entity rollover depth is deterministic, within
/// [64,127], and spreads evenly over the 64 buckets.
#[test]
fn rollover_depth_is_deterministic_bounded_and_even() {
    assert_eq!(rollover_depth("sh.600006"), rollover_depth("sh.600006"));
    // Pinned values guard cross-process / cross-version stability.
    assert_eq!(
        (rollover_depth("sh.600000"), rollover_depth("sz.000001")),
        (119, 117)
    );
    let mut counts = [0u32; 64];
    let mut n = 0u32;
    for exchange in ["sh", "sz"] {
        for code in 0..2500u32 {
            let depth = rollover_depth(&format!("{exchange}.{code:06}"));
            assert!((64..=127).contains(&depth));
            counts[(depth - 64) as usize] += 1;
            n += 1;
        }
    }
    assert_eq!(n, 5000);
    let mean = f64::from(n) / 64.0;
    let sigma = (f64::from(n) * (1.0 / 64.0) * (63.0 / 64.0)).sqrt();
    for (bucket, count) in counts.iter().enumerate() {
        assert!(
            (f64::from(*count) - mean).abs() <= 5.0 * sigma,
            "bucket {bucket} holds {count} (mean {mean:.1}, sigma {sigma:.1})"
        );
    }
    assert!(counts.iter().all(|c| *c > 0));
}

fn clips_parent(c: &Connection, version_id: &str) -> i64 {
    c.query_row(
        "SELECT version_key FROM data_versions WHERE version_id=?1",
        params![version_id],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn yearly_lifecycle_and_rejections() {
    let c = connection();
    let daily_bars = vec![
        cb_full(0, 100, 10, "100"),
        cb_full(1, 110, 20, "200"),
        cb_full(364, 120, 30, "300"),
    ];
    let (base_key, _) = publish_daily(&c, "daily-1", &daily_bars);
    let binding = put_binding(&c);
    let yearly_bars = vec![CompactBar {
        trade_day: 364,
        open_cents: 100,
        high_cents: 130,
        low_cents: 90,
        close_cents: 120,
        volume: Some(60),
        amount: Some("600".into()),
        r7_raw: None,
    }];
    let yseg = segment(&c, "sh.600006", Period::Year, &yearly_bars);
    let yearly_mk_draft = |version_id: &str, binding_digest: Digest| VersionDraft {
        version_id: version_id.into(),
        entity: "sh.600006".into(),
        period: Period::Year,
        source_binding_digest: binding_digest,
        coverage_status: CoverageStatus::Complete,
        evidence_digest: put_version_evidence(&c),
        created_at: "2026-09-21T00:00:00+00:00".into(),
        yearly_lineage: Some(YearlyLineageDraft {
            base_daily_version_key: base_key,
            algorithm_version: crate::yearly::ALGORITHM_VERSION.into(),
            first_trade_day: 0,
            last_trade_day: 364,
            as_of_day: 364,
        }),
    };
    let draft = yearly_mk_draft("yearly-1", binding);
    let key = stage_checkpoint(&c, &draft, &[full_clip(&yseg)], None).unwrap();
    mark_validated(&c, key).unwrap();
    publish(&c, key, ts()).unwrap();
    let verified = fixed(&c, "yearly-1").unwrap();
    assert_eq!(verified.version.period, Period::Year);
    assert_eq!(verified.version.records.len(), 1);
    let derivation = verified.version.yearly_derivation.clone().unwrap();
    assert_eq!(derivation.base_daily_version_id, "daily-1");
    assert_eq!(
        derivation.algorithm_version,
        crate::yearly::ALGORITHM_VERSION
    );
    assert_eq!(
        derivation.first_trading_date,
        NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()
    );
    assert_eq!(
        derivation.last_trading_date,
        NaiveDate::from_ymd_opt(1970, 12, 31).unwrap()
    );

    let mut bad = yearly_mk_draft("y-bad-period", binding);
    bad.yearly_lineage.as_mut().unwrap().base_daily_version_key = key;
    integrity_failure(stage_checkpoint(&c, &bad, &[full_clip(&yseg)], None));

    let (other_key, _) = {
        let seg = segment(&c, "sz.000002", Period::Day, &daily_bars);
        let draft_b = mk_draft("daily-other", "sz.000002", Period::Day, &c);
        let k = stage_checkpoint(&c, &draft_b, &[full_clip(&seg)], None).unwrap();
        mark_validated(&c, k).unwrap();
        publish(&c, k, ts()).unwrap();
        (k, ())
    };
    let mut bad = yearly_mk_draft("y-bad-entity", binding);
    bad.yearly_lineage.as_mut().unwrap().base_daily_version_key = other_key;
    integrity_failure(stage_checkpoint(&c, &bad, &[full_clip(&yseg)], None));

    let other_binding = {
        let evidence = put_evidence(
            &c,
            EvidenceKind::Generation,
            &serde_json::json!({"generation": "gen-2"}),
        )
        .unwrap();
        put_source_binding(
            &c,
            &SourceBinding {
                generation_id: "gen-2".into(),
                generation_manifest_sha256: [9; 32],
                day_member_sha256: [8; 32],
                security_event_sha256: [7; 32],
                evidence_digest: evidence,
            },
        )
        .unwrap()
    };
    integrity_failure(stage_checkpoint(
        &c,
        &yearly_mk_draft("y-bad-binding", other_binding),
        &[full_clip(&yseg)],
        None,
    ));

    let mut bad = yearly_mk_draft("y-bad-algorithm", binding);
    bad.yearly_lineage.as_mut().unwrap().algorithm_version = "other-v1".into();
    integrity_failure(stage_checkpoint(&c, &bad, &[full_clip(&yseg)], None));

    let mut bad = yearly_mk_draft("y-bad-bounds", binding);
    bad.yearly_lineage.as_mut().unwrap().last_trade_day = 365;
    integrity_failure(stage_checkpoint(&c, &bad, &[full_clip(&yseg)], None));

    let mut bad = yearly_mk_draft("y-bad-bounds2", binding);
    bad.yearly_lineage.as_mut().unwrap().first_trade_day = 365;
    integrity_failure(stage_checkpoint(&c, &bad, &[full_clip(&yseg)], None));

    let wrong_bars = vec![CompactBar {
        close_cents: 121,
        ..yearly_bars[0].clone()
    }];
    let wrong_seg = segment(&c, "sh.600006", Period::Year, &wrong_bars);
    integrity_failure(stage_checkpoint(
        &c,
        &yearly_mk_draft("y-bad-projection", binding),
        &[full_clip(&wrong_seg)],
        None,
    ));

    let mut bad = yearly_mk_draft("y-bad-base", binding);
    bad.yearly_lineage.as_mut().unwrap().base_daily_version_key = 9999;
    integrity_failure(stage_checkpoint(&c, &bad, &[full_clip(&yseg)], None));

    let unstaged = segment(&c, "sh.600006", Period::Day, &[cb_full(400, 100, 1, "1")]);
    let draft_unpub = mk_draft("daily-unpub", "sh.600006", Period::Day, &c);
    let unpub_key = stage_checkpoint(&c, &draft_unpub, &[full_clip(&unstaged)], None).unwrap();
    let mut bad = yearly_mk_draft("y-bad-unpub", binding);
    bad.yearly_lineage.as_mut().unwrap().base_daily_version_key = unpub_key;
    integrity_failure(stage_checkpoint(&c, &bad, &[full_clip(&yseg)], None));

    let mut bad = mk_draft("daily-lineage", "sh.600006", Period::Day, &c);
    bad.yearly_lineage = Some(YearlyLineageDraft {
        base_daily_version_key: base_key,
        algorithm_version: crate::yearly::ALGORITHM_VERSION.into(),
        first_trade_day: 0,
        last_trade_day: 364,
        as_of_day: 364,
    });
    let seg = segment(&c, "sh.600006", Period::Day, &[cb(500, 100)]);
    integrity_failure(stage_checkpoint(&c, &bad, &[full_clip(&seg)], None));

    let mut bad = yearly_mk_draft("y-no-lineage", binding);
    bad.yearly_lineage = None;
    integrity_failure(stage_checkpoint(&c, &bad, &[full_clip(&yseg)], None));

    lax(&c);
    c.execute(
        "UPDATE yearly_lineage SET last_trade_day=363 WHERE version_key=?1",
        params![key],
    )
    .unwrap();
    integrity_failure(fixed(&c, "yearly-1"));
    c.execute(
        "UPDATE yearly_lineage SET last_trade_day=364 WHERE version_key=?1",
        params![key],
    )
    .unwrap();
    c.execute(
        "DELETE FROM yearly_lineage WHERE version_key=?1",
        params![key],
    )
    .unwrap();
    integrity_failure(fixed(&c, "yearly-1"));
}

#[test]
fn list_metadata_only_and_ordering() {
    let c = connection();
    publish_daily(&c, "v1", &[cb(0, 100)]);
    publish_daily(&c, "v2", &[cb(0, 100), cb(5, 110)]);
    let summaries = list(&c, &mk_scope("sh.600006", Period::Day)).unwrap();
    assert_eq!(summaries.len(), 2);
    assert_eq!(summaries[0].version_id, "v2");
    assert_eq!(summaries[1].version_id, "v1");
    assert_eq!(summaries[1].record_count, 1);
    assert_eq!(summaries[0].source, "official");
    assert_eq!(summaries[0].integrity_sha256.len(), 64);

    let before = summaries.clone();
    c.execute("DELETE FROM segment_bars", []).unwrap();
    let after = list(&c, &mk_scope("sh.600006", Period::Day)).unwrap();
    assert_eq!(before, after);
    integrity_failure(fixed(&c, "v1"));

    let c = connection();
    publish_daily(&c, "v1", &[cb(0, 100)]);
    lax(&c);
    c.execute(
        "UPDATE data_versions SET record_count=9 WHERE version_id='v1'",
        [],
    )
    .unwrap();
    integrity_failure(list(&c, &mk_scope("sh.600006", Period::Day)));

    let c = connection();
    publish_daily(&c, "v1", &[cb(0, 100)]);
    lax(&c);
    c.execute(
        "UPDATE evidence_bundles SET canonical_json='{\"tampered\":1}'",
        [],
    )
    .unwrap();
    integrity_failure(list(&c, &mk_scope("sh.600006", Period::Day)));

    let c = connection();
    let (key, _) = publish_daily(&c, "v1", &[cb(0, 100)]);
    c.execute(
        "INSERT INTO yearly_lineage(version_key,base_daily_version_key,algorithm_version,first_trade_day,last_trade_day,as_of_day) VALUES(?1,?1,'local-natural-year-v1',0,0,0)",
        params![key],
    )
    .unwrap();
    integrity_failure(list(&c, &mk_scope("sh.600006", Period::Day)));
}

#[test]
fn bar_aware_unit_requirements() {
    let c = connection();
    let binding = put_binding(&c);
    let sparse_evidence = put_evidence(
        &c,
        EvidenceKind::Version,
        &version_evidence("official", false, false),
    )
    .unwrap();
    let seg = segment(&c, "sh.600006", Period::Day, &[cb(0, 100)]);
    let draft = VersionDraft {
        evidence_digest: sparse_evidence,
        ..mk_draft("v1", "sh.600006", Period::Day, &c)
    };
    let draft = VersionDraft {
        source_binding_digest: binding,
        ..draft
    };
    stage_checkpoint(&c, &draft, &[full_clip(&seg)], None).unwrap();

    let seg_vol = segment(&c, "sh.600006", Period::Day, &[cb_full(1, 100, 5, "9")]);
    let draft = VersionDraft {
        version_id: "v2".into(),
        evidence_digest: sparse_evidence,
        source_binding_digest: binding,
        ..mk_draft("x", "sh.600006", Period::Day, &c)
    };
    integrity_failure(stage_checkpoint(&c, &draft, &[full_clip(&seg_vol)], None));

    let mut no_amount = version_evidence("official", true, true);
    no_amount["unit_currency"]["amount_currency"] =
        serde_json::json!({"status":"Unknown","value":null,"reason":"not supplied"});
    let amount_unknown = put_evidence(&c, EvidenceKind::Version, &no_amount).unwrap();
    let draft = VersionDraft {
        version_id: "v3".into(),
        evidence_digest: amount_unknown,
        source_binding_digest: binding,
        ..mk_draft("x2", "sh.600006", Period::Day, &c)
    };
    integrity_failure(stage_checkpoint(&c, &draft, &[full_clip(&seg_vol)], None));

    let mut no_price = version_evidence("official", true, true);
    no_price["unit_currency"]["price_unit"] =
        serde_json::json!({"status":"Unknown","value":null,"reason":"not supplied"});
    let price_unknown = put_evidence(&c, EvidenceKind::Version, &no_price).unwrap();
    let draft = VersionDraft {
        version_id: "v4".into(),
        evidence_digest: price_unknown,
        source_binding_digest: binding,
        ..mk_draft("x3", "sh.600006", Period::Day, &c)
    };
    integrity_failure(stage_checkpoint(&c, &draft, &[full_clip(&seg)], None));
}

#[test]
fn leaf_bars_in_range_matches_full_reconstruction() {
    let c = connection();
    let bars: Vec<CompactBar> = (0..10).map(|d| cb(d, 100 + d)).collect();
    let (base_key, _) = publish_daily(&c, "v1", &bars);
    // upsert revises day 5 and appends days 10-12
    let rev = segment(
        &c,
        "sh.600006",
        Period::Day,
        &[cb(5, 555), cb(10, 200), cb(11, 210), cb(12, 220)],
    );
    let draft = mk_draft("v2", "sh.600006", Period::Day, &c);
    let key = stage_delta(&c, &draft, base_key, &[upsert(&rev, 5, 12)]).unwrap();
    mark_validated(&c, key).unwrap();
    publish(&c, key, ts()).unwrap();
    // remove days 2-3
    let draft = mk_draft("v3", "sh.600006", Period::Day, &c);
    let key = stage_delta(&c, &draft, key, &[remove(2, 3)]).unwrap();
    mark_validated(&c, key).unwrap();
    publish(&c, key, ts()).unwrap();

    let full = leaf_bars(&c, key).unwrap();
    for (start, end) in [(0, 12), (0, 0), (4, 7), (10, 12), (13, 20), (2, 2), (8, 9)] {
        let windowed = leaf_bars_in_range(&c, key, start, end).unwrap();
        let expected: Vec<CompactBar> = full
            .iter()
            .filter(|b| b.trade_day >= start && b.trade_day <= end)
            .cloned()
            .collect();
        assert_eq!(windowed, expected, "window [{start},{end}]");
    }
    // inverted range is empty, not an error
    assert!(leaf_bars_in_range(&c, key, 9, 5).unwrap().is_empty());
}

/// #259 P0-D: generation-common fields of `version` evidence are stored
/// once in `evidence_parts` and re-inlined on read — digests and canonical
/// bytes are unchanged externally.
#[test]
fn version_evidence_shared_fields_dedup_and_recompose() {
    let c = connection();
    let mut a = version_evidence("official", true, true);
    a["coverage_reason"] = serde_json::json!("entity-A window verified");
    let mut b = version_evidence("official", true, true);
    b["coverage_reason"] = serde_json::json!("entity-B window verified");
    let digest_a = put_evidence(&c, EvidenceKind::Version, &a).unwrap();
    let digest_b = put_evidence(&c, EvidenceKind::Version, &b).unwrap();
    assert_ne!(digest_a, digest_b);
    // Two parent rows; each shared field stored once for identical content.
    let bundles: i64 = c
        .query_row("SELECT count(*) FROM evidence_bundles", [], |r| r.get(0))
        .unwrap();
    let parts: i64 = c
        .query_row("SELECT count(*) FROM evidence_parts", [], |r| r.get(0))
        .unwrap();
    assert_eq!((bundles, parts), (2, 2));
    // Reads recompose to the byte-identical canonical document.
    let (kind, canonical) = read_evidence(&c, &digest_a).unwrap();
    assert_eq!(kind, EvidenceKind::Version);
    assert_eq!(
        canonical,
        compact_market::canonical_evidence(EvidenceKind::Version, &a)
            .unwrap()
            .canonical_json
    );
    // Re-putting identical evidence is idempotent and stores nothing new.
    assert_eq!(
        put_evidence(&c, EvidenceKind::Version, &a).unwrap(),
        digest_a
    );
    let parts_after: i64 = c
        .query_row("SELECT count(*) FROM evidence_parts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(parts_after, 2);
}

#[test]
fn version_evidence_part_tamper_or_loss_fails_closed() {
    let c = connection();
    let digest = put_version_evidence(&c);
    c.execute("UPDATE evidence_parts SET canonical_json='{\"x\":1}'", [])
        .unwrap();
    integrity_failure(read_evidence(&c, &digest));

    let c2 = connection();
    let digest2 = put_version_evidence(&c2);
    c2.execute("DELETE FROM evidence_parts", []).unwrap();
    integrity_failure(read_evidence(&c2, &digest2));
}

/// Rows written before decomposition existed carry the full document and
/// must still read back unchanged.
#[test]
fn version_evidence_undecomposed_row_still_reads() {
    let c = connection();
    let value = version_evidence("official", true, true);
    let bundle = compact_market::canonical_evidence(EvidenceKind::Version, &value).unwrap();
    c.execute(
        "INSERT INTO evidence_bundles VALUES(?1,'version',?2)",
        params![bundle.digest.as_slice(), bundle.canonical_json],
    )
    .unwrap();
    let (kind, canonical) = read_evidence(&c, &bundle.digest).unwrap();
    assert_eq!(kind, EvidenceKind::Version);
    assert_eq!(canonical, bundle.canonical_json);
}

// ======================= XP-018 =======================

/// Metadata assurance certifies the whole verified chain: touching the
/// delta RESULT first certifies its verified base ancestor — a later
/// base-facts touch is a certificate hit (one full-chain run total).
/// No legacy `Bar` records are projected for metadata assurance, and a
/// certificate never substitutes a whole-history demand: `verified`
/// still reconstructs real bars equal to the uncached `fixed` read.
#[test]
fn metadata_certificate_covers_verified_ancestors() {
    let c = connection();
    let (base_key, _) = publish_daily(&c, "v1", &[cb(0, 100), cb(1, 110)]);
    let extra = segment(&c, "sh.600006", Period::Day, &[cb(2, 120)]);
    let draft = mk_draft("v2", "sh.600006", Period::Day, &c);
    let delta_key = stage_delta(&c, &draft, base_key, &[upsert(&extra, 2, 2)]).unwrap();
    mark_validated(&c, delta_key).unwrap();
    publish(&c, delta_key, ts()).unwrap();

    let mut ctx = VerifyCtx::default();
    read_counts::reset();
    ctx.verified_facts(&c, delta_key).unwrap();
    ctx.verified_facts(&c, base_key).unwrap();
    let (_, fixed_count, _) = read_counts::snapshot();
    assert_eq!(fixed_count, 1, "result chain run covered the base ancestor");
    assert_eq!(ctx.fixed_runs, 1);
    assert_eq!(
        read_counts::snapshot_projections(),
        0,
        "metadata assurance built no legacy records"
    );
    // History demand still returns real fully-verified bars — the
    // metadata certificate is not an empty-history substitute.
    let (_, demand) = ctx.verified_by_id(&c, "v2").unwrap();
    assert_eq!(read_counts::snapshot_projections(), 1);
    let uncached = fixed(&c, "v2").unwrap();
    assert_eq!(demand.version, uncached.version);
    assert_eq!(demand.version.records.len(), 3);
}

/// A corrupt ancestor fails the RESULT's metadata verification and
/// installs no certificate: a base touch afterwards is a real counted
/// run that fails on the same corruption — never a phantom cert hit.
#[test]
fn corrupt_ancestor_blocks_result_and_leaves_no_certificate() {
    let c = connection();
    let (base_key, _) = publish_daily(&c, "v1", &[cb(0, 100), cb(1, 110)]);
    let extra = segment(&c, "sh.600006", Period::Day, &[cb(2, 120)]);
    let draft = mk_draft("v2", "sh.600006", Period::Day, &c);
    let delta_key = stage_delta(&c, &draft, base_key, &[upsert(&extra, 2, 2)]).unwrap();
    mark_validated(&c, delta_key).unwrap();
    publish(&c, delta_key, ts()).unwrap();
    lax(&c);
    c.execute(
        "UPDATE segment_bars SET close_cents=close_cents+1 WHERE segment_key IN \
         (SELECT segment_key FROM version_checkpoint_clips WHERE version_key=?1)",
        params![base_key],
    )
    .unwrap();

    let mut ctx = VerifyCtx::default();
    integrity_failure(ctx.verified_facts(&c, delta_key));
    assert_eq!(ctx.fixed_runs, 0);
    read_counts::reset();
    integrity_failure(ctx.verified_facts(&c, base_key));
    let (_, fixed, _) = read_counts::snapshot();
    assert_eq!(fixed, 1, "base touch ran real verification again");
    assert_eq!(ctx.fixed_runs, 0);
}

/// XP-018 streamed `read_segment`: a full 512-bar segment round-trips
/// verified (digest covers every bar); typed, bounds, digest and
/// missing corruption still fail closed; a second read through a
/// segment ctx is a same-snapshot hit while a fresh read after
/// corruption still fails.
#[test]
fn read_segment_streams_full_capacity_and_fails_closed() {
    let c = connection();
    let bars: Vec<CompactBar> = (0..512).map(|d| cb(d, 100 + d)).collect();
    let seg = segment(&c, "sh.600006", Period::Day, &bars);
    let stored = compact_v19::read_segment(&c, seg.key).unwrap();
    assert_eq!(stored.bars, bars);
    let mut sctx = compact_v19::SegmentReadCtx::default();
    let again = compact_v19::read_segment_ctx(&c, seg.key, &mut sctx).unwrap();
    assert_eq!(again.bars, bars);
    assert_eq!(sctx.stats().0, 1);
    let third = compact_v19::read_segment_ctx(&c, seg.key, &mut sctx).unwrap();
    assert_eq!(third.bars, bars);
    let fourth = compact_v19::read_segment_ctx(&c, seg.key, &mut sctx).unwrap();
    assert_eq!(fourth.bars, bars);
    assert_eq!(sctx.stats(), (1, 2, 1, sctx.stats().3));

    // Corruption classes on fresh fixtures: typed (non-canonical
    // amount), day bounds, stored digest, missing segment.
    lax(&c);
    c.execute(
        "UPDATE segment_bars SET amount='007' WHERE segment_key=?1 AND trade_day=10",
        params![seg.key],
    )
    .unwrap();
    integrity_failure(compact_v19::read_segment(&c, seg.key));

    let c = connection();
    let seg = segment(&c, "sh.600006", Period::Day, &bars);
    lax(&c);
    c.execute(
        "UPDATE bar_segments SET first_day=first_day+1 WHERE segment_key=?1",
        params![seg.key],
    )
    .unwrap();
    integrity_failure(compact_v19::read_segment(&c, seg.key));

    let c = connection();
    let seg = segment(&c, "sh.600006", Period::Day, &bars);
    lax(&c);
    c.execute(
        "UPDATE bar_segments SET segment_digest=?1 WHERE segment_key=?2",
        params![[0xabu8; 32].as_slice(), seg.key],
    )
    .unwrap();
    integrity_failure(compact_v19::read_segment(&c, seg.key));

    let c = connection();
    let seg = segment(&c, "sh.600006", Period::Day, &bars);
    lax(&c);
    c.execute(
        "DELETE FROM bar_segments WHERE segment_key=?1",
        params![seg.key],
    )
    .unwrap();
    integrity_failure(compact_v19::read_segment(&c, seg.key));
}

/// XP-018 delta differential: `apply_delta_scoped` (dispatcher) output
/// equals the verbatim original digest-check + map path on every shape;
/// the eligibility predicate is asserted per case so equality never
/// silently degenerates to "same code twice".
#[test]
fn apply_delta_append_only_matches_map_reference() {
    /// The ORIGINAL `apply_delta_scoped` body, verbatim: delta_digest
    /// validation then the BTreeMap upsert/remove loop.
    fn reference_apply(
        c: &Connection,
        base: &[CompactBar],
        ops: &[StoredDelta],
    ) -> Result<Vec<CompactBar>, Error> {
        let plain = delta_ops(ops);
        if !plain.is_empty() {
            compact_market::delta_digest(&plain).map_err(integrity)?;
        }
        apply_delta_map(c, base, ops, "sh.600006", Period::Day, None)
    }
    let compare =
        |c: &Connection, base: &[CompactBar], ops: &[StoredDelta], eligible: bool, label: &str| {
            assert_eq!(
                delta_is_append_only(base, ops),
                eligible,
                "eligibility: {label}"
            );
            let fast = apply_delta_scoped(c, base, ops, "sh.600006", Period::Day, None);
            let oracle = reference_apply(c, base, ops);
            match (&fast, &oracle) {
                (Ok(a), Ok(b)) => assert_eq!(a, b, "output divergence: {label}"),
                (Err(_), Err(_)) => {}
                _ => panic!("path divergence {label}: fast={fast:?} oracle={oracle:?}"),
            }
            (fast, oracle)
        };
    let c = connection();

    // Eligible shapes: empty / one-bar / 512-bar bases, tail-only
    // upserts, multi-op gaps, and sparse clip contents.
    let u = |c: &Connection, days: &[i64], start: i64, end: i64| {
        let seg = segment(
            c,
            "sh.600006",
            Period::Day,
            &days.iter().map(|&d| cb(d, 100)).collect::<Vec<_>>(),
        );
        upsert(&seg, start, end)
    };
    let (fast, oracle) = compare(&c, &[], &[u(&c, &[3, 4, 5], 3, 5)], true, "empty base");
    assert_eq!(
        fast.unwrap()
            .iter()
            .map(|b| b.trade_day)
            .collect::<Vec<_>>(),
        vec![3, 4, 5]
    );
    let _ = oracle;
    let _ = compare(
        &c,
        &[cb(0, 100)],
        &[u(&c, &[1, 2], 1, 2)],
        true,
        "base-1 tail upsert",
    );
    let base512: Vec<CompactBar> = (0..512).map(|d| cb(d, 100)).collect();
    let _ = compare(
        &c,
        &base512,
        &[u(&c, &[512, 513, 514], 512, 514)],
        true,
        "base-512 tail upsert",
    );
    let _ = compare(
        &c,
        &[cb(0, 100), cb(1, 110), cb(2, 120)],
        &[u(&c, &[10, 11], 10, 11), u(&c, &[20, 21], 20, 21)],
        true,
        "multi-op disjoint gaps",
    );
    // Missing supplied days inside the upsert range: only the supplied
    // days are inserted — no filling, identical on both paths.
    let sparse = segment(&c, "sh.600006", Period::Day, &[cb(5, 100), cb(8, 100)]);
    let (fast, oracle) = compare(
        &c,
        &[cb(0, 100), cb(1, 110)],
        &[upsert(&sparse, 5, 8)],
        true,
        "sparse clip inside range",
    );
    let fast = fast.unwrap();
    assert_eq!(
        fast.iter().map(|b| b.trade_day).collect::<Vec<_>>(),
        vec![0, 1, 5, 8]
    );
    assert_eq!(fast, oracle.unwrap());

    // Fallback shapes: overwrite overlap, exact-boundary, Remove, mixed
    // ops, unsorted and duplicate bases.
    let overwrite = segment(
        &c,
        "sh.600006",
        Period::Day,
        &[cb(1, 999), cb(2, 999), cb(3, 999)],
    );
    let (fast, oracle) = compare(
        &c,
        &[cb(0, 100), cb(1, 110), cb(2, 120)],
        &[upsert(&overwrite, 1, 3)],
        false,
        "overwrite overlapping range",
    );
    let fast = fast.unwrap();
    assert_eq!(
        fast.iter()
            .map(|b| (b.trade_day, b.close_cents))
            .collect::<Vec<_>>(),
        vec![(0, 100), (1, 999), (2, 999), (3, 999)]
    );
    assert_eq!(fast, oracle.unwrap());
    let boundary = segment(&c, "sh.600006", Period::Day, &[cb(2, 999), cb(3, 130)]);
    let _ = compare(
        &c,
        &[cb(0, 100), cb(1, 110), cb(2, 120)],
        &[upsert(&boundary, 2, 3)],
        false,
        "exact boundary start == base last",
    );
    let _ = compare(
        &c,
        &[cb(0, 100), cb(1, 110), cb(2, 120)],
        &[remove(1, 2)],
        false,
        "remove op",
    );
    let _ = compare(
        &c,
        &[cb(0, 100), cb(1, 110)],
        &[u(&c, &[3, 4], 3, 4), remove(5, 6)],
        false,
        "mixed upsert+remove",
    );
    let _ = compare(
        &c,
        &[cb(0, 100), cb(2, 120), cb(1, 110)],
        &[u(&c, &[3, 4], 3, 4)],
        false,
        "unsorted base",
    );
    let _ = compare(
        &c,
        &[cb(0, 100), cb(0, 100), cb(1, 110)],
        &[u(&c, &[3, 4], 3, 4)],
        false,
        "duplicate base days",
    );

    // Tampered stored content rejects BOTH paths — the fast path never
    // returns unverified appended data.
    let c2 = connection();
    let tampered = segment(&c2, "sh.600006", Period::Day, &[cb(3, 100), cb(4, 100)]);
    let op = upsert(&tampered, 3, 4);
    lax(&c2);
    c2.execute(
        "UPDATE segment_bars SET close_cents=close_cents+1 WHERE segment_key=?1",
        params![tampered.key],
    )
    .unwrap();
    let base = [cb(0, 100), cb(1, 110)];
    assert!(delta_is_append_only(&base, std::slice::from_ref(&op)));
    integrity_failure(apply_delta_scoped(
        &c2,
        &base,
        std::slice::from_ref(&op),
        "sh.600006",
        Period::Day,
        None,
    ));
    integrity_failure(reference_apply(&c2, &base, std::slice::from_ref(&op)));
    integrity_failure(apply_delta_append(
        &c2,
        &base,
        std::slice::from_ref(&op),
        "sh.600006",
        Period::Day,
        None,
    ));

    // Clip row_count that contradicts the segment rejects both paths
    // (clip-level check runs on every use, cache or not).
    let c3 = connection();
    let seg3 = segment(&c3, "sh.600006", Period::Day, &[cb(3, 100), cb(4, 100)]);
    let bad_clip = StoredDelta {
        range_start_day: 3,
        range_end_day: 4,
        kind: StoredDeltaKind::Upsert(clip(&seg3, 3, 4, 99)),
    };
    let base = [cb(0, 100)];
    assert!(delta_is_append_only(&base, std::slice::from_ref(&bad_clip)));
    integrity_failure(apply_delta_scoped(
        &c3,
        &base,
        std::slice::from_ref(&bad_clip),
        "sh.600006",
        Period::Day,
        None,
    ));
    integrity_failure(reference_apply(&c3, &base, std::slice::from_ref(&bad_clip)));
}

// ---- XP-025 G1: streamed chain rebuild equals the per-node-clone reference ----

fn chain_keys(c: &Connection, depth: i64, full_every: i64) -> Vec<i64> {
    let (root, _) = publish_daily(c, "v0", &[cb(0, 100)]);
    let mut keys = vec![root];
    for i in 1..=depth {
        let bar = if full_every > 0 && i % full_every == 0 {
            cb_full(i, 100 + i, i, "5")
        } else {
            cb(i, 100 + i)
        };
        let seg = segment(c, "sh.600006", Period::Day, &[bar]);
        let draft = mk_draft(&format!("v{i}"), "sh.600006", Period::Day, c);
        let key = stage_delta(c, &draft, *keys.last().unwrap(), &[upsert(&seg, i, i)]).unwrap();
        mark_validated(c, key).unwrap();
        publish(c, key, ts()).unwrap();
        keys.push(key);
    }
    keys
}

/// Streamed verification must reproduce, node by node, exactly what the
/// per-node-clone reference reconstructs and verifies.
fn assert_stream_matches_reference(c: &Connection, key: i64) {
    let reference = reconstruct_version_reference(c, key).unwrap();
    let mut index = 0;
    let (leaf_row, leaf_bars) = walk_chain_scoped(c, key, None, |row, bars, summary| {
        let (ref_row, ref_bars) = &reference[index];
        assert_eq!(row.key, ref_row.key, "node {index} row");
        assert_eq!(bars, ref_bars.as_slice(), "node {index} bars");
        assert_eq!(*summary, BarsSummary::of(bars), "node {index} summary");
        index += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(index, reference.len());
    let (ref_leaf, ref_leaf_bars) = reference.last().unwrap();
    assert_eq!(leaf_row.key, ref_leaf.key);
    assert_eq!(&leaf_bars, ref_leaf_bars);
    let verified = verified_chain_scoped(c, key, None).unwrap();
    assert_eq!(verified.leaf_bars, *ref_leaf_bars);
    assert_eq!(verified.nodes.len(), reference.len());
    for ((row, evidence), (ref_row, ref_bars)) in verified.nodes.iter().zip(&reference) {
        assert_eq!(row.key, ref_row.key);
        assert_eq!(
            *evidence,
            verify_published_node(c, ref_row, ref_bars).unwrap(),
            "node evidence"
        );
    }
}

#[test]
fn streamed_chain_matches_reference_at_depths_0_1_64_256_and_copies_stay_linear() {
    let c = connection();
    let keys = chain_keys(&c, 256, 7);
    for depth in [0usize, 1, 64, 256] {
        assert_stream_matches_reference(&c, keys[depth]);
        BAR_COPIES.with(|n| n.set(0));
        let verified = verified_chain_scoped(&c, keys[depth], None).unwrap();
        let copies = BAR_COPIES.with(|n| n.get());
        // Root (1 bar) + one appended bar per delta == final bar count: no
        // per-node copy of the growing projection (the old path copied
        // O(bars x depth), ~depth^2/2 here).
        assert_eq!(verified.leaf_bars.len(), depth + 1);
        assert_eq!(copies, depth + 1, "bar copies at depth {depth}");
    }
}

#[test]
fn streamed_chain_matches_reference_for_overwrite_and_remove_deltas() {
    let c = connection();
    let (mut parent, _) =
        publish_daily(&c, "v0", &[cb(0, 100), cb(1, 101), cb(2, 102), cb(3, 103)]);
    // Overwrite an existing day, remove a day, then append: the first two
    // are not append-only (map path), the last returns to the fast path.
    let overwrite = segment(&c, "sh.600006", Period::Day, &[cb_full(2, 999, 5, "7")]);
    let append = segment(&c, "sh.600006", Period::Day, &[cb(10, 110)]);
    let steps: Vec<(&str, Vec<StoredDelta>)> = vec![
        ("v1", vec![upsert(&overwrite, 2, 2)]),
        ("v2", vec![remove(1, 1)]),
        ("v3", vec![upsert(&append, 10, 10)]),
    ];
    let mut keys = vec![parent];
    for (id, ops) in steps {
        let draft = mk_draft(id, "sh.600006", Period::Day, &c);
        let key = stage_delta(&c, &draft, parent, &ops).unwrap();
        mark_validated(&c, key).unwrap();
        publish(&c, key, ts()).unwrap();
        parent = key;
        keys.push(key);
    }
    for key in keys {
        assert_stream_matches_reference(&c, key);
    }
}
