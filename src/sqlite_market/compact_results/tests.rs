use rust_decimal::Decimal;
use sha2::Digest as _;

use super::super::compact_v19::StoredSegment;
use super::super::compact_versions::{
    self as versions, CompactVersionState, VersionDraft, YearlyLineageDraft,
};
use super::*;
use crate::compact_market::SourceBinding;
use crate::market_data::CoverageStatus;

fn connection() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch(
        "CREATE TABLE evidence_bundles(
           evidence_digest BLOB PRIMARY KEY CHECK(length(evidence_digest)=32),
           evidence_kind TEXT NOT NULL CHECK(evidence_kind IN ('generation','batch_selection','version','continuity','catalog','yearly')),
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
         ) STRICT;
         CREATE TABLE result_registry(
           result_id TEXT PRIMARY KEY,
           result_kind INTEGER NOT NULL CHECK(result_kind IN (1,2))
         ) STRICT, WITHOUT ROWID;
         CREATE TABLE tasks(
           task_id TEXT PRIMARY KEY,
           status TEXT NOT NULL CHECK(status IN ('pending','running','cancel_requested','needs_verification','succeeded','partially_failed','failed','cancelled','interrupted')),
           state_revision INTEGER NOT NULL CHECK(state_revision>=0),
           item_count INTEGER NOT NULL DEFAULT 0 CHECK(item_count>=0),
           items_pending INTEGER NOT NULL DEFAULT 0 CHECK(items_pending>=0),
           items_running INTEGER NOT NULL DEFAULT 0 CHECK(items_running>=0),
           items_finished INTEGER NOT NULL DEFAULT 0 CHECK(items_finished>=0),
           items_succeeded INTEGER NOT NULL DEFAULT 0 CHECK(items_succeeded>=0)
         ) STRICT;
         CREATE TABLE task_items(
           task_id TEXT NOT NULL REFERENCES tasks(task_id),
           item_id TEXT NOT NULL,
           stage TEXT NOT NULL CHECK(stage IN ('pending','running','finished')),
           outcome_json TEXT,
           PRIMARY KEY(task_id,item_id),
           CHECK((stage='finished')=(outcome_json IS NOT NULL))
         ) STRICT;
         CREATE TABLE market_update_results(
           result_key INTEGER PRIMARY KEY,
           result_id TEXT NOT NULL UNIQUE REFERENCES result_registry(result_id),
           task_id TEXT NOT NULL,
           item_id TEXT NOT NULL,
           base_version_key INTEGER NOT NULL REFERENCES data_versions(version_key),
           result_version_key INTEGER NOT NULL REFERENCES data_versions(version_key),
           outcome INTEGER NOT NULL CHECK(outcome IN (1,2)),
           range_start_day INTEGER NOT NULL,
           range_end_day INTEGER NOT NULL,
           algorithm_version TEXT NOT NULL,
           source_binding_digest BLOB NOT NULL REFERENCES source_bindings(source_binding_digest),
           continuity_mode INTEGER NOT NULL CHECK(continuity_mode IN (1,2,3)),
           prefix_bytes INTEGER,
           base_projection_digest BLOB NOT NULL CHECK(length(base_projection_digest)=32),
           result_projection_digest BLOB NOT NULL CHECK(length(result_projection_digest)=32),
           delta_digest BLOB CHECK(delta_digest IS NULL OR length(delta_digest)=32),
           evidence_digest BLOB NOT NULL REFERENCES evidence_bundles(evidence_digest),
           task_revision INTEGER NOT NULL,
           result_digest BLOB NOT NULL CHECK(length(result_digest)=32),
           UNIQUE(task_id,item_id),
           FOREIGN KEY(task_id,item_id) REFERENCES task_items(task_id,item_id),
           CHECK(range_start_day <= range_end_day),
           CHECK((continuity_mode IN (1,2) AND prefix_bytes IS NOT NULL AND prefix_bytes>=0)
              OR (continuity_mode=3 AND prefix_bytes IS NULL)),
           CHECK((outcome=1 AND continuity_mode=1 AND base_version_key=result_version_key AND delta_digest IS NULL)
              OR (outcome=2 AND continuity_mode IN (2,3) AND base_version_key<>result_version_key AND delta_digest IS NOT NULL))
         ) STRICT;
         CREATE TABLE market_update_delta_clips(
           result_key INTEGER NOT NULL REFERENCES market_update_results(result_key),
           seq INTEGER NOT NULL CHECK(seq >= 0),
           range_start_day INTEGER NOT NULL,
           range_end_day INTEGER NOT NULL,
           segment_key INTEGER NOT NULL REFERENCES bar_segments(segment_key),
           clip_start_day INTEGER NOT NULL,
           clip_end_day INTEGER NOT NULL,
           row_count INTEGER NOT NULL CHECK(row_count > 0),
           PRIMARY KEY(result_key,seq),
           CHECK(range_start_day <= range_end_day),
           CHECK(clip_start_day <= clip_end_day)
         ) STRICT, WITHOUT ROWID;
         CREATE TABLE publication_results(
           result_key INTEGER PRIMARY KEY,
           publication_id TEXT NOT NULL UNIQUE REFERENCES result_registry(result_id),
           task_id TEXT NOT NULL,
           item_id TEXT NOT NULL,
           publication_kind INTEGER NOT NULL CHECK(publication_kind IN (1,2,3)),
           base_version_key INTEGER REFERENCES data_versions(version_key),
           result_version_key INTEGER NOT NULL REFERENCES data_versions(version_key),
           outcome INTEGER NOT NULL CHECK(outcome IN (1,2)),
           algorithm_version TEXT NOT NULL,
           source_binding_digest BLOB NOT NULL REFERENCES source_bindings(source_binding_digest),
           evidence_digest BLOB NOT NULL REFERENCES evidence_bundles(evidence_digest),
           task_revision INTEGER NOT NULL,
           result_digest BLOB NOT NULL CHECK(length(result_digest)=32),
           UNIQUE(task_id,item_id),
           FOREIGN KEY(task_id,item_id) REFERENCES task_items(task_id,item_id),
           CHECK((outcome=1 AND base_version_key IS NOT NULL AND base_version_key=result_version_key)
              OR (outcome=2 AND (base_version_key IS NULL OR base_version_key<>result_version_key)))
         ) STRICT;
         CREATE TABLE task_publications(
           task_id TEXT NOT NULL REFERENCES tasks(task_id),
           version_key INTEGER NOT NULL UNIQUE REFERENCES data_versions(version_key),
           result_id TEXT NOT NULL UNIQUE REFERENCES result_registry(result_id),
           PRIMARY KEY(task_id,version_key)
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
        volume: Some(volume),
        amount: Some(amount.into()),
        r7_raw: Some(7),
        ..cb(day, cents)
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

fn unit(status: &str, value: Value) -> Value {
    serde_json::json!({"status": status, "value": value, "reason": "bound"})
}

fn version_evidence(source: &str) -> Value {
    serde_json::json!({
        "source": source,
        "coverage_reason": "full history verified",
        "unit_currency": {
            "price_currency": unit("Confirmed", serde_json::json!("CNY")),
            "price_unit": unit("Confirmed", serde_json::json!("share")),
            "volume_unit": unit("Confirmed", serde_json::json!("share")),
            "amount_currency": unit("Confirmed", serde_json::json!("CNY")),
            "amount_unit": unit("Confirmed", serde_json::json!("yuan"))
        },
        "adjustment_evidence": unit("Confirmed", serde_json::json!("qfq"))
    })
}

fn put_version_evidence(c: &Connection) -> Digest {
    versions::put_evidence(c, EvidenceKind::Version, &version_evidence("official")).unwrap()
}

fn put_continuity_evidence(c: &Connection) -> Digest {
    versions::put_evidence(
        c,
        EvidenceKind::Continuity,
        &serde_json::json!({"summary": "no gaps"}),
    )
    .unwrap()
}

fn put_yearly_evidence(c: &Connection) -> Digest {
    versions::put_evidence(
        c,
        EvidenceKind::Yearly,
        &serde_json::json!({"yearly": "derived"}),
    )
    .unwrap()
}

fn put_binding(c: &Connection, generation: &str) -> Digest {
    let evidence = versions::put_evidence(
        c,
        EvidenceKind::Generation,
        &serde_json::json!({"generation": generation}),
    )
    .unwrap();
    versions::put_source_binding(
        c,
        &SourceBinding {
            generation_id: generation.into(),
            generation_manifest_sha256: [1; 32],
            day_member_sha256: [2; 32],
            security_event_sha256: [3; 32],
            evidence_digest: evidence,
        },
    )
    .unwrap()
}

fn mk_draft(c: &Connection, version_id: &str, entity: &str, period: Period) -> VersionDraft {
    VersionDraft {
        version_id: version_id.into(),
        entity: entity.into(),
        period,
        source_binding_digest: put_binding(c, "gen-1"),
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

fn publish_daily(c: &Connection, version_id: &str, bars: &[CompactBar]) -> i64 {
    let seg = segment(c, "sh.600006", Period::Day, bars);
    let draft = mk_draft(c, version_id, "sh.600006", Period::Day);
    let key = versions::stage_checkpoint(c, &draft, &[full_clip(&seg)], None).unwrap();
    versions::mark_validated(c, key).unwrap();
    versions::publish(c, key, ts()).unwrap();
    key
}

fn staged_daily_delta(c: &Connection, version_id: &str, parent: i64, ops: &[StoredDelta]) -> i64 {
    let draft = mk_draft(c, version_id, "sh.600006", Period::Day);
    let key = versions::stage_delta(c, &draft, parent, ops).unwrap();
    versions::mark_validated(c, key).unwrap();
    key
}

fn create_task(c: &Connection, task_id: &str, revision: i64, running_items: &[&str]) {
    c.execute(
        "INSERT INTO tasks(task_id,status,state_revision,item_count,items_pending,items_running,items_finished,items_succeeded) VALUES(?1,'running',?2,?3,0,?3,0,0)",
        params![task_id, revision, running_items.len() as i64],
    )
    .unwrap();
    for item in running_items {
        c.execute(
            "INSERT INTO task_items(task_id,item_id,stage) VALUES(?1,?2,'running')",
            params![task_id, item],
        )
        .unwrap();
    }
}

fn update_command(
    c: &Connection,
    result_id: &str,
    base_key: i64,
    result_key: i64,
    change: MarketUpdateChange,
) -> MarketUpdateCommit {
    MarketUpdateCommit {
        result_id: result_id.into(),
        task_id: "task-1".into(),
        item_id: "item-1".into(),
        base_version_key: base_key,
        result_version_key: result_key,
        range_start_day: 0,
        range_end_day: 10,
        algorithm_version: "market-update-v1".into(),
        source_binding_digest: versions::result_facts(c, result_key)
            .map(|facts| facts.source_binding_digest)
            .unwrap_or_else(|_| put_binding(c, "gen-1")),
        evidence_digest: put_continuity_evidence(c),
        expected_task_revision: 1,
        published_at: None,
        head: HeadExpectation::Keep,
        change,
    }
}

fn table_counts(c: &Connection) -> Vec<i64> {
    [
        "bar_segments",
        "segment_bars",
        "version_checkpoint_clips",
        "version_delta_clips",
        "data_versions",
        "publication_results",
        "task_publications",
        "market_update_delta_clips",
    ]
    .iter()
    .map(|table| {
        c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    })
    .collect()
}

fn task_row(c: &Connection, task_id: &str) -> (String, i64, i64, i64, i64, i64) {
    c.query_row(
        "SELECT status,state_revision,items_pending,items_running,items_finished,items_succeeded FROM tasks WHERE task_id=?1",
        params![task_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
    )
    .unwrap()
}

fn item_row(c: &Connection, task_id: &str, item_id: &str) -> (String, Option<String>) {
    c.query_row(
        "SELECT stage,outcome_json FROM task_items WHERE task_id=?1 AND item_id=?2",
        params![task_id, item_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .unwrap()
}

fn integrity_failure<T>(result: Result<T, Error>) {
    match result {
        Err(Error::Integrity(_)) => {}
        Err(e) => panic!("expected Integrity failure, got {e}"),
        Ok(_) => panic!("expected Integrity failure, got Ok"),
    }
}

fn conflict_failure<T>(result: Result<T, Error>) {
    match result {
        Err(Error::Conflict(_)) => {}
        Err(e) => panic!("expected Conflict failure, got {e}"),
        Ok(_) => panic!("expected Conflict failure, got Ok"),
    }
}

#[test]
fn no_change_market_update_commit() {
    let mut c = connection();
    let base = publish_daily(&c, "v1", &[cb(0, 100), cb(1, 110)]);
    create_task(&c, "task-1", 1, &["item-1"]);
    let before = table_counts(&c);
    let command = update_command(
        &c,
        "result-1",
        base,
        base,
        MarketUpdateChange::NoChange { prefix_bytes: 64 },
    );
    let stored = commit_market_update(&mut c, &command).unwrap();
    assert_eq!(table_counts(&c), before);
    assert_eq!(stored.base_version_key, base);
    assert_eq!(stored.result_version_key, base);
    assert_eq!(stored.identity.outcome, UpdateOutcome::NoChange);
    assert_eq!(stored.identity.continuity_mode, ContinuityMode::Unchanged);
    assert_eq!(stored.identity.task_revision, 2);

    let registry: i64 = c
        .query_row("SELECT count(*) FROM result_registry", [], |r| r.get(0))
        .unwrap();
    assert_eq!(registry, 1);
    let (status, revision, pending, running, finished, succeeded) = task_row(&c, "task-1");
    assert_eq!(status, "succeeded");
    assert_eq!(
        (revision, pending, running, finished, succeeded),
        (2, 0, 0, 1, 1)
    );
    let (stage, outcome) = item_row(&c, "task-1", "item-1");
    assert_eq!(stage, "finished");
    let outcome = outcome.unwrap();
    assert!(outcome.len() <= 192);
    assert_eq!(
        outcome,
        "{\"error\":null,\"evidence\":{\"result_id\":\"result-1\",\"result_kind\":\"market_update\"},\"success\":true}"
    );
    let payloads: i64 = c
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name LIKE '%payload%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(payloads, 0);
    let read = read_market_update(&c, "result-1").unwrap();
    assert_eq!(read.result_key, stored.result_key);
    assert_eq!(read.result_digest, stored.result_digest);
}

#[test]
fn no_change_update_accepts_different_source_binding() {
    let mut c = connection();
    let base = publish_daily(&c, "v1", &[cb(0, 100)]);
    create_task(&c, "task-1", 1, &["item-1"]);
    let other_binding = put_binding(&c, "gen-2");
    let command = MarketUpdateCommit {
        source_binding_digest: other_binding,
        ..update_command(
            &c,
            "result-1",
            base,
            base,
            MarketUpdateChange::NoChange { prefix_bytes: 8 },
        )
    };
    commit_market_update(&mut c, &command).unwrap();
    let verified = versions::fixed(&c, "v1").unwrap();
    assert_eq!(verified.version.records.len(), 1);
}

#[test]
fn append_market_update_delta_commit() {
    let mut c = connection();
    let base = publish_daily(&c, "v1", &[cb(0, 100), cb(1, 110), cb(2, 120)]);
    let extra = segment(&c, "sh.600006", Period::Day, &[cb(3, 130), cb(4, 140)]);
    let ops = vec![upsert(&extra, 3, 4)];
    let result_key = staged_daily_delta(&c, "v2", base, &ops);
    create_task(&c, "task-1", 1, &["item-1"]);
    c.execute(
        "INSERT INTO effective_heads(entity,period,adjustment,version_key) VALUES('sh.600006',1,1,?1)",
        params![base],
    )
    .unwrap();
    let command = MarketUpdateCommit {
        published_at: Some(ts()),
        head: HeadExpectation::Version(base),
        ..update_command(
            &c,
            "result-1",
            base,
            result_key,
            MarketUpdateChange::Append {
                prefix_bytes: 96,
                ops: ops.clone(),
            },
        )
    };
    let stored = commit_market_update(&mut c, &command).unwrap();
    assert_eq!(stored.result_version_key, result_key);
    assert_eq!(stored.identity.continuity_mode, ContinuityMode::Append);
    assert_eq!(stored.identity.prefix_bytes, Some(96));
    assert_eq!(
        stored.identity.delta_digest,
        Some(compact_market::delta_digest(&codec_ops(&ops)).unwrap())
    );
    let clip_rows: i64 = c
        .query_row(
            "SELECT count(*) FROM market_update_delta_clips WHERE result_key=?1",
            params![stored.result_key],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(clip_rows, 1);
    let head: i64 = c
        .query_row(
            "SELECT version_key FROM effective_heads WHERE entity='sh.600006'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(head, result_key);
    let publications: i64 = c
        .query_row("SELECT count(*) FROM task_publications", [], |r| r.get(0))
        .unwrap();
    assert_eq!(publications, 1);
    let verified = versions::fixed(&c, "v2").unwrap();
    assert_eq!(verified.version.records.len(), 5);
    assert_eq!(verified.version.records[4].close, Decimal::new(140, 2));
    let base_verified = versions::fixed(&c, "v1").unwrap();
    assert_eq!(base_verified.version.records.len(), 3);
    let read = read_market_update(&c, "result-1").unwrap();
    assert_eq!(read.result_digest, stored.result_digest);
}

#[test]
fn append_market_update_checkpoint_rollover() {
    let mut c = connection();
    let mut parent = publish_daily(&c, "v0", &[cb(0, 100)]);
    for i in 1..=256i64 {
        let seg = segment(&c, "sh.600006", Period::Day, &[cb(i, 100 + i)]);
        parent = staged_daily_delta(&c, &format!("v{i}"), parent, &[upsert(&seg, i, i)]);
        versions::publish(&c, parent, ts()).unwrap();
    }
    let next = segment(&c, "sh.600006", Period::Day, &[cb(257, 357)]);
    let ops = vec![upsert(&next, 257, 257)];
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
    let draft = mk_draft(&c, "v257", "sh.600006", Period::Day);
    let result_key = versions::stage_checkpoint(
        &c,
        &draft,
        &clips,
        Some(&versions::CheckpointBasis {
            parent_version_key: parent,
            delta_ops: ops.clone(),
        }),
    )
    .unwrap();
    versions::mark_validated(&c, result_key).unwrap();
    create_task(&c, "task-1", 1, &["item-1"]);
    let command = MarketUpdateCommit {
        published_at: Some(ts()),
        head: HeadExpectation::Keep,
        ..update_command(
            &c,
            "result-1",
            parent,
            result_key,
            MarketUpdateChange::Append {
                prefix_bytes: 512,
                ops,
            },
        )
    };
    let stored = commit_market_update(&mut c, &command).unwrap();
    let clip_rows: i64 = c
        .query_row(
            "SELECT count(*) FROM market_update_delta_clips WHERE result_key=?1",
            params![stored.result_key],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(clip_rows, 1);
    let read = read_market_update(&c, "result-1").unwrap();
    assert_eq!(read.result_digest, stored.result_digest);
    let verified = versions::fixed(&c, "v257").unwrap();
    assert_eq!(verified.version.records.len(), 258);
}

#[test]
fn full_revision_market_update_commit() {
    let mut c = connection();
    let base = publish_daily(&c, "v1", &[cb(0, 100), cb(1, 110)]);
    let revised = segment(&c, "sh.600006", Period::Day, &[cb(0, 105), cb(1, 115)]);
    let draft = mk_draft(&c, "v2", "sh.600006", Period::Day);
    let result_key = versions::stage_checkpoint(&c, &draft, &[full_clip(&revised)], None).unwrap();
    versions::mark_validated(&c, result_key).unwrap();
    create_task(&c, "task-1", 1, &["item-1"]);
    let command = MarketUpdateCommit {
        published_at: Some(ts()),
        ..update_command(&c, "result-1", base, result_key, MarketUpdateChange::Full)
    };
    let stored = commit_market_update(&mut c, &command).unwrap();
    let base_facts = versions::result_facts(&c, base).unwrap();
    let result_facts = versions::result_facts(&c, result_key).unwrap();
    assert_eq!(
        stored.identity.delta_digest,
        Some(compact_market::full_revision_digest(
            base_facts.projection_digest,
            result_facts.projection_digest
        ))
    );
    assert_eq!(stored.identity.continuity_mode, ContinuityMode::Full);
    assert_eq!(stored.identity.prefix_bytes, None);
    let clip_rows: i64 = c
        .query_row("SELECT count(*) FROM market_update_delta_clips", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(clip_rows, 0);
    let base_verified = versions::fixed(&c, "v1").unwrap();
    assert_eq!(base_verified.version.records[0].close, Decimal::new(100, 2));
    let read = read_market_update(&c, "result-1").unwrap();
    assert_eq!(read.result_digest, stored.result_digest);
}

#[test]
fn publication_commits() {
    let mut c = connection();
    let base = publish_daily(&c, "v1", &[cb_full(0, 100, 10, "100")]);
    let binding = versions::result_facts(&c, base)
        .unwrap()
        .source_binding_digest;

    let yearly_bars = vec![CompactBar {
        trade_day: 0,
        open_cents: 100,
        high_cents: 110,
        low_cents: 90,
        close_cents: 100,
        volume: Some(10),
        amount: Some("100".into()),
        r7_raw: None,
    }];
    let yseg = segment(&c, "sh.600006", Period::Year, &yearly_bars);
    let yearly_draft = VersionDraft {
        period: Period::Year,
        yearly_lineage: Some(YearlyLineageDraft {
            base_daily_version_key: base,
            algorithm_version: crate::yearly::ALGORITHM_VERSION.into(),
            first_trade_day: 0,
            last_trade_day: 0,
            as_of_day: 0,
        }),
        ..mk_draft(&c, "y1", "sh.600006", Period::Year)
    };
    let yearly_key =
        versions::stage_checkpoint(&c, &yearly_draft, &[full_clip(&yseg)], None).unwrap();
    versions::mark_validated(&c, yearly_key).unwrap();

    create_task(
        &c,
        "task-pub",
        5,
        &["i-day", "i-year", "i-repub", "i-nochange", "i-bad"],
    );
    let publication = |id: &str,
                       item: &str,
                       kind: PublicationKind,
                       base_key: Option<i64>,
                       result_key: i64,
                       outcome: UpdateOutcome,
                       evidence: Digest,
                       head: HeadExpectation,
                       expected_revision: u64| {
        PublicationCommit {
            publication_id: id.into(),
            task_id: "task-pub".into(),
            item_id: item.into(),
            publication_kind: kind,
            base_version_key: base_key,
            result_version_key: result_key,
            outcome,
            algorithm_version: "collection-v1".into(),
            source_binding_digest: binding,
            evidence_digest: evidence,
            expected_task_revision: expected_revision,
            published_at: Some(ts()),
            head,
        }
    };

    let daily2 = segment(
        &c,
        "sh.600006",
        Period::Day,
        &[cb_full(0, 100, 10, "100"), cb_full(1, 110, 20, "200")],
    );
    let draft2 = mk_draft(&c, "v2", "sh.600006", Period::Day);
    let result2 = versions::stage_checkpoint(&c, &draft2, &[full_clip(&daily2)], None).unwrap();
    versions::mark_validated(&c, result2).unwrap();

    let command = publication(
        "pub-1",
        "i-day",
        PublicationKind::Collection,
        Some(base),
        result2,
        UpdateOutcome::Published,
        put_version_evidence(&c),
        HeadExpectation::Version(base),
        5,
    );
    conflict_failure(commit_publication(&mut c, &command).map(|s| s.result_key));
    c.execute(
        "INSERT INTO effective_heads(entity,period,adjustment,version_key) VALUES('sh.600006',1,1,?1)",
        params![base],
    )
    .unwrap();
    let stored = commit_publication(&mut c, &command).unwrap();
    assert_eq!(
        stored.identity.publication_kind,
        PublicationKind::Collection
    );
    assert_eq!(stored.identity.task_revision, 6);
    let head: i64 = c
        .query_row(
            "SELECT version_key FROM effective_heads WHERE entity='sh.600006' AND period=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(head, result2);
    assert!(read_publication(&c, "pub-1").is_ok());

    let command = publication(
        "pub-2",
        "i-year",
        PublicationKind::Yearly,
        None,
        yearly_key,
        UpdateOutcome::Published,
        put_yearly_evidence(&c),
        HeadExpectation::Missing,
        6,
    );
    let stored = commit_publication(&mut c, &command).unwrap();
    assert_eq!(stored.identity.base_version_id, None);
    let head: i64 = c
        .query_row(
            "SELECT version_key FROM effective_heads WHERE entity='sh.600006' AND period=2",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(head, yearly_key);
    assert!(read_publication(&c, "pub-2").is_ok());

    let seg3 = segment(
        &c,
        "sh.600006",
        Period::Day,
        &[cb_full(0, 100, 10, "100"), cb_full(1, 110, 20, "200")],
    );
    let draft3 = mk_draft(&c, "v3", "sh.600006", Period::Day);
    let republish_result =
        versions::stage_checkpoint(&c, &draft3, &[full_clip(&seg3)], None).unwrap();
    versions::mark_validated(&c, republish_result).unwrap();
    let command = publication(
        "pub-3",
        "i-repub",
        PublicationKind::Republish,
        Some(result2),
        republish_result,
        UpdateOutcome::Published,
        put_version_evidence(&c),
        HeadExpectation::Keep,
        7,
    );
    let stored = commit_publication(&mut c, &command).unwrap();
    assert_eq!(stored.identity.base_version_id, Some("v2".to_string()));
    assert!(read_publication(&c, "pub-3").is_ok());

    let mut command = publication(
        "pub-4",
        "i-nochange",
        PublicationKind::Collection,
        Some(result2),
        result2,
        UpdateOutcome::NoChange,
        put_version_evidence(&c),
        HeadExpectation::Keep,
        8,
    );
    command.published_at = None;
    let stored = commit_publication(&mut c, &command).unwrap();
    assert_eq!(stored.identity.outcome, UpdateOutcome::NoChange);
    let publications: i64 = c
        .query_row("SELECT count(*) FROM task_publications", [], |r| r.get(0))
        .unwrap();
    assert_eq!(publications, 3);
    assert!(read_publication(&c, "pub-4").is_ok());

    let bad = publication(
        "pub-5",
        "i-bad",
        PublicationKind::Collection,
        None,
        yearly_key,
        UpdateOutcome::Published,
        put_version_evidence(&c),
        HeadExpectation::Keep,
        9,
    );
    integrity_failure(commit_publication(&mut c, &bad).map(|s| s.result_key));
    let bad = publication(
        "pub-6",
        "i-bad",
        PublicationKind::Yearly,
        None,
        yearly_key,
        UpdateOutcome::Published,
        put_version_evidence(&c),
        HeadExpectation::Keep,
        9,
    );
    integrity_failure(commit_publication(&mut c, &bad).map(|s| s.result_key));
}

#[test]
fn rollback_on_conflicts() {
    let mut c = connection();
    let base = publish_daily(&c, "v1", &[cb(0, 100)]);
    create_task(
        &c,
        "task-1",
        1,
        &["item-1", "item-2", "item-3", "item-4", "item-5"],
    );
    c.execute(
        "INSERT INTO result_registry(result_id,result_kind) VALUES('taken-pub',2)",
        [],
    )
    .unwrap();
    let command = update_command(
        &c,
        "taken-pub",
        base,
        base,
        MarketUpdateChange::NoChange { prefix_bytes: 1 },
    );
    conflict_failure(commit_market_update(&mut c, &command));
    assert_eq!(item_row(&c, "task-1", "item-1").0, "running");
    assert_eq!(task_row(&c, "task-1").1, 1);

    let command = update_command(
        &c,
        "result-1",
        base,
        base,
        MarketUpdateChange::NoChange { prefix_bytes: 1 },
    );
    commit_market_update(&mut c, &command).unwrap();
    let dup = update_command(
        &c,
        "result-2",
        base,
        base,
        MarketUpdateChange::NoChange { prefix_bytes: 1 },
    );
    conflict_failure(commit_market_update(&mut c, &dup));

    let mut big = update_command(
        &c,
        &"x".repeat(200),
        base,
        base,
        MarketUpdateChange::NoChange { prefix_bytes: 1 },
    );
    big.item_id = "item-2".into();
    integrity_failure(commit_market_update(&mut c, &big));
    assert_eq!(item_row(&c, "task-1", "item-2").0, "running");

    let mut stale = update_command(
        &c,
        "result-3",
        base,
        base,
        MarketUpdateChange::NoChange { prefix_bytes: 1 },
    );
    stale.item_id = "item-3".into();
    stale.expected_task_revision = 99;
    conflict_failure(commit_market_update(&mut c, &stale));

    let mut not_running = update_command(
        &c,
        "result-4",
        base,
        base,
        MarketUpdateChange::NoChange { prefix_bytes: 1 },
    );
    not_running.item_id = "item-1".into();
    conflict_failure(commit_market_update(&mut c, &not_running));

    let extra = segment(&c, "sh.600006", Period::Day, &[cb(1, 110)]);
    let ops = vec![upsert(&extra, 1, 1)];
    let result_key = staged_daily_delta(&c, "v2", base, &ops);
    let mut head_conflict = MarketUpdateCommit {
        item_id: "item-4".into(),
        published_at: Some(ts()),
        head: HeadExpectation::Version(base),
        ..update_command(
            &c,
            "result-5",
            base,
            result_key,
            MarketUpdateChange::Append {
                prefix_bytes: 1,
                ops,
            },
        )
    };
    head_conflict.item_id = "item-4".into();
    conflict_failure(commit_market_update(&mut c, &head_conflict));
    let facts = versions::result_facts(&c, result_key).unwrap();
    assert_eq!(facts.state, CompactVersionState::Validated);
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM result_registry WHERE result_id='result-5'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}

#[test]
fn read_verification_fails_closed() {
    let mut c = connection();
    let base = publish_daily(&c, "v1", &[cb(0, 100), cb(1, 110)]);
    let extra = segment(&c, "sh.600006", Period::Day, &[cb(2, 120)]);
    let ops = vec![upsert(&extra, 2, 2)];
    let result_key = staged_daily_delta(&c, "v2", base, &ops);
    create_task(&c, "task-1", 1, &["item-1"]);
    let command = MarketUpdateCommit {
        published_at: Some(ts()),
        ..update_command(
            &c,
            "result-1",
            base,
            result_key,
            MarketUpdateChange::Append {
                prefix_bytes: 1,
                ops,
            },
        )
    };
    let stored = commit_market_update(&mut c, &command).unwrap();
    assert!(read_market_update(&c, "result-1").is_ok());
    assert!(matches!(
        read_market_update(&c, "absent"),
        Err(Error::NotFound)
    ));

    let tamper = |sql: &str| {
        let mut c = connection();
        let base = publish_daily(&c, "v1", &[cb(0, 100), cb(1, 110)]);
        let extra = segment(&c, "sh.600006", Period::Day, &[cb(2, 120)]);
        let ops = vec![upsert(&extra, 2, 2)];
        let result_key = staged_daily_delta(&c, "v2", base, &ops);
        create_task(&c, "task-1", 1, &["item-1"]);
        let command = MarketUpdateCommit {
            published_at: Some(ts()),
            ..update_command(
                &c,
                "result-1",
                base,
                result_key,
                MarketUpdateChange::Append {
                    prefix_bytes: 1,
                    ops,
                },
            )
        };
        commit_market_update(&mut c, &command).unwrap();
        lax(&c);
        c.execute(sql, []).unwrap();
        integrity_failure(read_market_update(&c, "result-1"));
    };
    tamper("UPDATE result_registry SET result_kind=2 WHERE result_id='result-1'");
    tamper(
        "UPDATE market_update_results SET result_digest=x'0000000000000000000000000000000000000000000000000000000000000000' WHERE result_id='result-1'",
    );
    tamper(
        "UPDATE market_update_results SET base_projection_digest=x'0000000000000000000000000000000000000000000000000000000000000000' WHERE result_id='result-1'",
    );
    tamper("UPDATE market_update_results SET task_revision=99 WHERE result_id='result-1'");
    tamper(
        "UPDATE market_update_results SET source_binding_digest=x'0000000000000000000000000000000000000000000000000000000000000000' WHERE result_id='result-1'",
    );
    tamper(
        "UPDATE market_update_results SET evidence_digest=x'0000000000000000000000000000000000000000000000000000000000000000' WHERE result_id='result-1'",
    );
    tamper("DELETE FROM market_update_delta_clips");
    tamper("UPDATE market_update_delta_clips SET range_start_day=9");
    tamper("UPDATE task_items SET outcome_json='{\"success\":true}'");
    tamper("UPDATE tasks SET state_revision=0 WHERE task_id='task-1'");

    c.execute(
        "INSERT INTO effective_heads(entity,period,adjustment,version_key) VALUES('sh.600006',1,1,?1)",
        params![result_key],
    )
    .unwrap();
    let read = read_market_update(&c, "result-1").unwrap();
    assert_eq!(read.result_key, stored.result_key);
}

#[test]
fn read_market_linkage_fails_closed() {
    fn setup() -> (Connection, i64, i64, StoredMarketUpdateResult) {
        let mut c = connection();
        let base = publish_daily(&c, "v1", &[cb(0, 100), cb(1, 110)]);
        let extra = segment(&c, "sh.600006", Period::Day, &[cb(2, 120)]);
        let ops = vec![upsert(&extra, 2, 2)];
        let result_key = staged_daily_delta(&c, "v2", base, &ops);
        create_task(&c, "task-1", 1, &["item-1"]);
        let command = MarketUpdateCommit {
            published_at: Some(ts()),
            ..update_command(
                &c,
                "result-1",
                base,
                result_key,
                MarketUpdateChange::Append {
                    prefix_bytes: 1,
                    ops,
                },
            )
        };
        let stored = commit_market_update(&mut c, &command).unwrap();
        (c, base, result_key, stored)
    }

    let (c, _, _, stored) = setup();
    let mut identity = stored.identity.clone();
    identity.base_projection_digest = [0xab; 32];
    let digest = compact_market::market_update_result_digest(&identity).unwrap();
    c.execute(
        "UPDATE market_update_results SET base_projection_digest=?1,result_digest=?2 WHERE result_id='result-1'",
        params![[0xabu8; 32].as_slice(), digest.as_slice()],
    )
    .unwrap();
    integrity_failure(read_market_update(&c, "result-1"));

    let (c, _, _, stored) = setup();
    let other = put_binding(&c, "gen-2");
    let mut identity = stored.identity.clone();
    identity.source_binding_digest = other;
    let digest = compact_market::market_update_result_digest(&identity).unwrap();
    c.execute(
        "UPDATE market_update_results SET source_binding_digest=?1,result_digest=?2 WHERE result_id='result-1'",
        params![other.as_slice(), digest.as_slice()],
    )
    .unwrap();
    integrity_failure(read_market_update(&c, "result-1"));

    let (c, _, _, _) = setup();
    let alt = segment(&c, "sh.600006", Period::Day, &[cb(5, 150)]);
    c.execute(
        "UPDATE market_update_delta_clips SET segment_key=?1,clip_start_day=5,clip_end_day=5",
        params![alt.key],
    )
    .unwrap();
    integrity_failure(read_market_update(&c, "result-1"));

    let (c, _, result_key, _) = setup();
    c.execute(
        "DELETE FROM version_delta_clips WHERE version_key=?1",
        params![result_key],
    )
    .unwrap();
    integrity_failure(read_market_update(&c, "result-1"));

    let (c, _, _, _) = setup();
    c.execute("DELETE FROM task_publications", []).unwrap();
    integrity_failure(read_market_update(&c, "result-1"));
}

#[test]
fn read_publication_fails_closed() {
    fn setup() -> (Connection, StoredPublicationResult) {
        let mut c = connection();
        let base = publish_daily(&c, "v1", &[cb(0, 100)]);
        let seg = segment(&c, "sh.600006", Period::Day, &[cb(0, 100), cb(1, 110)]);
        let draft = mk_draft(&c, "v2", "sh.600006", Period::Day);
        let result_key = versions::stage_checkpoint(&c, &draft, &[full_clip(&seg)], None).unwrap();
        versions::mark_validated(&c, result_key).unwrap();
        create_task(&c, "task-1", 1, &["item-1"]);
        let binding = versions::result_facts(&c, result_key)
            .unwrap()
            .source_binding_digest;
        let command = PublicationCommit {
            publication_id: "pub-1".into(),
            task_id: "task-1".into(),
            item_id: "item-1".into(),
            publication_kind: PublicationKind::Collection,
            base_version_key: Some(base),
            result_version_key: result_key,
            outcome: UpdateOutcome::Published,
            algorithm_version: "collection-v1".into(),
            source_binding_digest: binding,
            evidence_digest: put_version_evidence(&c),
            expected_task_revision: 1,
            published_at: Some(ts()),
            head: HeadExpectation::Keep,
        };
        let stored = commit_publication(&mut c, &command).unwrap();
        (c, stored)
    }

    let (c, _) = setup();
    c.execute(
        "UPDATE result_registry SET result_kind=1 WHERE result_id='pub-1'",
        [],
    )
    .unwrap();
    integrity_failure(read_publication(&c, "pub-1"));

    let (c, _) = setup();
    c.execute(
        "UPDATE publication_results SET result_digest=x'0000000000000000000000000000000000000000000000000000000000000000' WHERE publication_id='pub-1'",
        [],
    )
    .unwrap();
    integrity_failure(read_publication(&c, "pub-1"));

    let (c, stored) = setup();
    let other = put_binding(&c, "gen-2");
    let mut identity = stored.identity.clone();
    identity.source_binding_digest = other;
    let digest = compact_market::publication_result_digest(&identity).unwrap();
    c.execute(
        "UPDATE publication_results SET source_binding_digest=?1,result_digest=?2 WHERE publication_id='pub-1'",
        params![other.as_slice(), digest.as_slice()],
    )
    .unwrap();
    integrity_failure(read_publication(&c, "pub-1"));

    let (c, stored) = setup();
    let wrong_kind = put_continuity_evidence(&c);
    let mut identity = stored.identity.clone();
    identity.evidence_digest = wrong_kind;
    let digest = compact_market::publication_result_digest(&identity).unwrap();
    c.execute(
        "UPDATE publication_results SET evidence_digest=?1,result_digest=?2 WHERE publication_id='pub-1'",
        params![wrong_kind.as_slice(), digest.as_slice()],
    )
    .unwrap();
    integrity_failure(read_publication(&c, "pub-1"));

    let (c, _) = setup();
    c.execute("DELETE FROM task_publications", []).unwrap();
    integrity_failure(read_publication(&c, "pub-1"));

    let (c, _) = setup();
    c.execute(
        "UPDATE task_items SET outcome_json='{\"success\":true}'",
        [],
    )
    .unwrap();
    integrity_failure(read_publication(&c, "pub-1"));
}

#[test]
fn overflow_inputs_rejected_before_effects() {
    let mut c = connection();
    let base = publish_daily(&c, "v1", &[cb(0, 100)]);
    let extra = segment(&c, "sh.600006", Period::Day, &[cb(1, 110)]);
    let ops = vec![upsert(&extra, 1, 1)];
    let result_key = staged_daily_delta(&c, "v2", base, &ops);
    create_task(&c, "task-1", 1, &["item-1", "item-2", "item-3"]);

    let mut command = MarketUpdateCommit {
        published_at: Some(ts()),
        ..update_command(
            &c,
            "result-1",
            base,
            result_key,
            MarketUpdateChange::Append {
                prefix_bytes: 1,
                ops: ops.clone(),
            },
        )
    };
    command.expected_task_revision = u64::MAX;
    integrity_failure(commit_market_update(&mut c, &command));
    command.expected_task_revision = i64::MAX as u64 + 5;
    integrity_failure(commit_market_update(&mut c, &command));
    command.expected_task_revision = 1;
    command.result_id = "result-2".into();
    command.item_id = "item-2".into();
    command.change = MarketUpdateChange::NoChange {
        prefix_bytes: u64::MAX,
    };
    command.result_version_key = base;
    command.published_at = None;
    integrity_failure(commit_market_update(&mut c, &command));

    let facts = versions::result_facts(&c, result_key).unwrap();
    assert_eq!(facts.state, CompactVersionState::Validated);
    let registry: i64 = c
        .query_row("SELECT count(*) FROM result_registry", [], |r| r.get(0))
        .unwrap();
    assert_eq!(registry, 0);
    assert_eq!(item_row(&c, "task-1", "item-1").0, "running");
    assert_eq!(item_row(&c, "task-1", "item-2").0, "running");
    assert_eq!(task_row(&c, "task-1").1, 1);
}

#[test]
fn illegal_update_combinations_rejected() {
    let mut c = connection();
    let base = publish_daily(&c, "v1", &[cb(0, 100), cb(1, 110)]);
    create_task(&c, "task-1", 1, &["i1", "i2", "i3", "i4", "i5", "i6", "i7"]);

    let remove = StoredDelta {
        range_start_day: 2,
        range_end_day: 2,
        kind: StoredDeltaKind::Remove,
    };
    let command = update_command(
        &c,
        "r-remove",
        base,
        base,
        MarketUpdateChange::Append {
            prefix_bytes: 1,
            ops: vec![remove],
        },
    );
    let mut command = MarketUpdateCommit {
        item_id: "i1".into(),
        ..command
    };
    integrity_failure(commit_market_update(&mut c, &command));

    command.result_id = "r-empty".into();
    command.item_id = "i2".into();
    command.change = MarketUpdateChange::Append {
        prefix_bytes: 1,
        ops: vec![],
    };
    integrity_failure(commit_market_update(&mut c, &command));

    let old = segment(&c, "sh.600006", Period::Day, &[cb(0, 200)]);
    command.result_id = "r-overlap".into();
    command.item_id = "i3".into();
    command.change = MarketUpdateChange::Append {
        prefix_bytes: 1,
        ops: vec![upsert(&old, 0, 0)],
    };
    integrity_failure(commit_market_update(&mut c, &command));

    let extra = segment(&c, "sh.600006", Period::Day, &[cb(2, 120)]);
    let delta_key = staged_daily_delta(&c, "v2", base, &[upsert(&extra, 2, 2)]);
    command.result_id = "r-full-delta".into();
    command.item_id = "i4".into();
    command.result_version_key = delta_key;
    command.published_at = Some(ts());
    command.change = MarketUpdateChange::Full;
    integrity_failure(commit_market_update(&mut c, &command));

    command.result_id = "r-nochange-publish".into();
    command.item_id = "i5".into();
    command.result_version_key = base;
    command.published_at = Some(ts());
    command.change = MarketUpdateChange::NoChange { prefix_bytes: 1 };
    integrity_failure(commit_market_update(&mut c, &command));

    command.result_id = "r-bad-evidence".into();
    command.item_id = "i6".into();
    command.published_at = None;
    command.evidence_digest = put_version_evidence(&c);
    integrity_failure(commit_market_update(&mut c, &command));

    command.result_id = "r-missing-head".into();
    command.item_id = "i7".into();
    command.evidence_digest = put_continuity_evidence(&c);
    command.result_version_key = delta_key;
    command.published_at = Some(ts());
    command.head = HeadExpectation::Missing;
    command.change = MarketUpdateChange::Append {
        prefix_bytes: 1,
        ops: vec![upsert(&extra, 2, 2)],
    };
    conflict_failure(commit_market_update(&mut c, &command));

    let nochange_pub = PublicationCommit {
        publication_id: "p-bad".into(),
        task_id: "task-1".into(),
        item_id: "i7".into(),
        publication_kind: PublicationKind::Collection,
        base_version_key: None,
        result_version_key: base,
        outcome: UpdateOutcome::NoChange,
        algorithm_version: "collection-v1".into(),
        source_binding_digest: put_binding(&c, "gen-1"),
        evidence_digest: put_version_evidence(&c),
        expected_task_revision: 1,
        published_at: None,
        head: HeadExpectation::Keep,
    };
    integrity_failure(commit_publication(&mut c, &nochange_pub).map(|s| s.result_key));
}

#[test]
#[ignore = "#259 S3 retained production-shaped no-change"]
fn retained_s3_no_change_adds_only_typed_receipt() {
    let retained = std::path::Path::new("/home/box/retained-inputs");
    let inputs = [
        (
            "official-base.zip",
            "base.zip",
            "base_industry",
            "http://www.tdx.com.cn/products/data/data/dbf/base.zip",
            "dcdf6b49898926675698263535021aed7ad92b34be82a1a9e8a2a8982bd68bc6",
        ),
        (
            "official-hsjday.zip",
            "hsjday.zip",
            "day",
            "https://data.tdx.com.cn/vipdoc/hsjday.zip",
            "c82f852a7bc2e52c7e89347a1e0d1878b01ce997946c2745f7af323752270892",
        ),
        (
            "official-gbbq.zip",
            "gbbq.zip",
            "gbbq",
            "http://www.tdx.com.cn/products/data/data/dbf/gbbq.zip",
            "daad6290c4f1cd478d7436b1e7077260a3e3e52cefd7eae58e4f24878fa2d0e9",
        ),
    ];
    if inputs.iter().any(|(src, ..)| !retained.join(src).is_file()) {
        println!("retained inputs absent; skipped");
        return;
    }
    let dir = tempfile::tempdir_in("/home/box").unwrap();
    let mut files = Vec::new();
    for (src, name, role, url, expected_sha) in inputs {
        let path = dir.path().join(name);
        std::fs::hard_link(retained.join(src), &path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let digest: Digest = sha2::Sha256::digest(&bytes).into();
        assert_eq!(compact_market::digest_hex(&digest), expected_sha);
        files.push(serde_json::json!({
            "role": role,
            "relative_path": name,
            "source_url": url,
            "acquisition_started_at": "2026-09-11T01:00:00Z",
            "acquisition_completed_at": "2026-09-11T02:00:00Z",
            "last_modified": null,
            "etag": null,
            "bytes": bytes.len(),
            "sha256": compact_market::digest_hex(&digest),
        }));
    }
    let manifest = serde_json::to_vec(
        &serde_json::json!({"schema":"x-data-mcp.official-file-input.v1","files":files}),
    )
    .unwrap();
    let now: DateTime<Utc> = "2026-09-12T00:00:00Z".parse().unwrap();
    let source =
        crate::official_market::TdxMarketSource::inspect(dir.path(), &manifest, now).unwrap();
    let batch = source.materialize_daily("sz.000002").unwrap();
    assert_eq!(batch.bars.len(), 8479);

    let mut c = connection();
    let bars: Vec<CompactBar> = batch
        .bars
        .iter()
        .map(|bar| CompactBar::from_bar(bar, Period::Day).unwrap())
        .collect();
    let mut clips = Vec::new();
    for chunk in bars.chunks(compact_market::MAX_SEGMENT_BARS) {
        let seg = segment(&c, "sz.000002", Period::Day, chunk);
        clips.push(full_clip(&seg));
    }
    let draft = mk_draft(&c, "retained-v1", "sz.000002", Period::Day);
    let key = versions::stage_checkpoint(&c, &draft, &clips, None).unwrap();
    versions::mark_validated(&c, key).unwrap();
    versions::publish(&c, key, ts()).unwrap();
    create_task(&c, "task-1", 1, &["item-1"]);
    let before = table_counts(&c);
    let command = MarketUpdateCommit {
        range_start_day: bars.first().unwrap().trade_day,
        range_end_day: bars.last().unwrap().trade_day,
        ..update_command(
            &c,
            "result-1",
            key,
            key,
            MarketUpdateChange::NoChange {
                prefix_bytes: 8479 * crate::day_file::RECORD_BYTES as u64,
            },
        )
    };
    let stored = commit_market_update(&mut c, &command).unwrap();
    let read = read_market_update(&c, "result-1").unwrap();
    assert_eq!(read.result_digest, stored.result_digest);
    let after = table_counts(&c);
    assert_eq!(before, after);
    let registry: i64 = c
        .query_row("SELECT count(*) FROM result_registry", [], |r| r.get(0))
        .unwrap();
    let updates: i64 = c
        .query_row("SELECT count(*) FROM market_update_results", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!((registry, updates), (1, 1));
    let payloads: i64 = c
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name LIKE '%payload%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(payloads, 0);
    let (_, outcome) = item_row(&c, "task-1", "item-1");
    let outcome = outcome.unwrap();
    assert!(outcome.len() <= 192);
    assert!(!outcome.contains("bars") && !outcome.contains("records"));
    let delta_null: i64 = c
        .query_row(
            "SELECT count(*) FROM market_update_results WHERE delta_digest IS NULL AND base_version_key=result_version_key",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(delta_null, 1);
    let before_map: serde_json::Value = [
        "bar_segments",
        "segment_bars",
        "version_checkpoint_clips",
        "version_delta_clips",
        "data_versions",
        "publication_results",
        "task_publications",
        "market_update_delta_clips",
    ]
    .iter()
    .enumerate()
    .map(|(index, table)| (table.to_string(), serde_json::json!(before[index])))
    .collect();
    let after_map: serde_json::Value = [
        "bar_segments",
        "segment_bars",
        "version_checkpoint_clips",
        "version_delta_clips",
        "data_versions",
        "publication_results",
        "task_publications",
        "market_update_delta_clips",
    ]
    .iter()
    .enumerate()
    .map(|(index, table)| (table.to_string(), serde_json::json!(after[index])))
    .collect();
    println!(
        "S3_RETAINED_RESULT={}",
        serde_json::json!({
            "symbol": "sz.000002",
            "bars": batch.bars.len(),
            "segment_count": clips.len(),
            "core_counts_before": before_map,
            "core_counts_after": after_map,
            "result_registry_added": registry,
            "market_update_results_added": updates,
            "outcome_json_bytes": outcome.len(),
            "payloads_table_exists": false,
            "no_change": true,
            "decision": "PASS",
        })
    );
}
