//! Schema-21 write-path tests (SPEC §8.1 items 4/5/6/16, §8.2 items
//! 12/13/14/15, plus the fold-equivalence invariant §8.1-1/2/3).
use std::collections::BTreeSet;
use std::path::PathBuf;

use serde_json::{Value, json};

use super::*;
use crate::compact_market::{Digest, EvidenceKind, SourceBinding, VersionEvidence};
use crate::market_data::Provenance;
use crate::market_data::{
    Adjustment, AdjustmentEvidence, Bar, CoverageEvidence, EvidenceValue, MarketDataBatch,
    UnitCurrencyEvidence,
};
use crate::market_storage::{MarketRepository, Scope};
use crate::market_storage::{Publication, PublishIntent, PublishPlan, compat};
use crate::market_update::publication::RangeRepository;
use crate::publish::{EffectiveVersion, ValidationSummary};
use crate::sqlite_market::{series, storage};
use crate::task_control::{self, CommandKey, ItemSpec};
use chrono::{DateTime, NaiveDate, Utc};
use rusqlite::Connection;

fn tempdir() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/dev/shm")
        .unwrap()
}

fn repo(dir: &tempfile::TempDir) -> (PathBuf, super::super::compact_repo::SqliteCompactMarket) {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.path().join("market.db");
    crate::sqlite_tasks::SqliteTasks::new(&path)
        .initialize_compact()
        .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let repo = super::super::compact_repo::SqliteCompactMarket::open(&path).unwrap();
    (path, repo)
}

fn ts() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-09-21T00:00:00+00:00")
        .unwrap()
        .with_timezone(&Utc)
}

fn date(day: i64) -> NaiveDate {
    crate::compact_market::epoch_date(day).unwrap()
}

fn bar(day: i64, cents: i64) -> Bar {
    crate::compact_market::CompactBar {
        trade_day: day,
        open_cents: cents,
        high_cents: cents + 10,
        low_cents: cents - 10,
        close_cents: cents,
        volume: Some(100),
        amount: Some("1000".into()),
        r7_raw: Some(7),
    }
    .to_bar(Period::Day)
    .unwrap()
}

fn units() -> UnitCurrencyEvidence {
    UnitCurrencyEvidence {
        price_currency: EvidenceValue::confirmed("CNY", "bound"),
        price_unit: EvidenceValue::confirmed("share", "bound"),
        volume_unit: EvidenceValue::confirmed("share", "bound"),
        amount_currency: EvidenceValue::confirmed("CNY", "bound"),
        amount_unit: EvidenceValue::confirmed("yuan", "bound"),
    }
}

fn version_evidence() -> Value {
    serde_json::to_value(VersionEvidence {
        source: "official".into(),
        coverage_reason: "full history verified".into(),
        unit_currency: units(),
        adjustment_evidence: AdjustmentEvidence::confirmed("qfq", "bound"),
    })
    .unwrap()
}

fn generation_evidence() -> Value {
    json!({
        "generation_id": "gen-1",
        "snapshot_inputs": [{
            "role": "daily",
            "member": "sh.600006.day",
            "archive_sha256": "12".repeat(32),
            "member_sha256": "cd".repeat(32)
        }]
    })
}

fn put_seed_material(c: &Connection) -> (Digest, Digest) {
    let version_ev =
        evidence_store::put_evidence(c, EvidenceKind::Version, &version_evidence()).unwrap();
    let gen_ev =
        evidence_store::put_evidence(c, EvidenceKind::Generation, &generation_evidence()).unwrap();
    let binding = evidence_store::put_source_binding(
        c,
        &SourceBinding {
            generation_id: "gen-1".into(),
            generation_manifest_sha256: [0x0b; 32],
            day_member_sha256: [0x0c; 32],
            security_event_sha256: [0x0e; 32],
            evidence_digest: gen_ev,
        },
    )
    .unwrap();
    (version_ev, binding)
}

const ITEM: &str = "item-1";
const RUN: &str = "run";
const SYMBOL: &str = "sh.600006";

/// Accept + claim + start a one-item daily task bound to `SYMBOL` — the
/// same task machinery the production runner drives.
fn running_task(repo: &super::super::compact_repo::SqliteCompactMarket) -> (String, u64) {
    use crate::task_control::TaskRepository;
    let scope = task_control::legacy::scope_id(SYMBOL, "day");
    let work = task_control::FrozenWork {
        items: vec![ItemSpec {
            item_id: ITEM.into(),
            target: json!({"symbol": SYMBOL, "period": "day"}),
            progress_unit: "bars".into(),
            frozen_inputs: json!({}),
            scope_keys: BTreeSet::from([scope.clone()]),
        }],
        scopes: BTreeSet::from([scope]),
        shared_evidence: None,
    };
    let command = task_control::Command {
        key: CommandKey {
            operation: "collect_data".into(),
            dataset: "market.daily".into(),
            contract_version: "v1".into(),
        },
        intent: json!({}),
    };
    let acceptance = repo.accept("req-1", &command, &work, RUN, None).unwrap();
    let task_id = match acceptance.record.outcome {
        task_control::RequestOutcome::Task(snapshot) => snapshot.task.task_id.clone(),
        _ => panic!("expected task outcome"),
    };
    let revision = repo.task(&task_id).unwrap().task.state_revision;
    let revision = repo
        .transition(&task_id, revision, RUN, task_control::Change::Claim)
        .unwrap()
        .task
        .state_revision;
    let revision = repo
        .transition(
            &task_id,
            revision,
            RUN,
            task_control::Change::StartItem(ITEM.into()),
        )
        .unwrap()
        .task
        .state_revision;
    (task_id, revision)
}

fn command(task_id: &str, publication_id: &str, revision: u64) -> Publication {
    Publication {
        publication_id: publication_id.into(),
        version_id: "candidate-label".into(),
        task_id: task_id.into(),
        item_id: ITEM.into(),
        run_id: RUN.into(),
        expected_revision: revision,
        intent: PublishIntent::Daily {
            expected_head: None,
        },
    }
}

fn scope() -> Scope {
    Scope {
        symbol: SYMBOL.into(),
        period: Period::Day,
    }
}

/// Commit a first-publish envelope commit: stage → candidate →
/// mark_validated → commit — same flow the envelope path runs.
fn publish_bars(
    repo: &super::super::compact_repo::SqliteCompactMarket,
    task_id: &str,
    revision: u64,
    publication_id: &str,
    version_label: &str,
    bars: Vec<Bar>,
) -> crate::market_storage::Receipt {
    let mut version = EffectiveVersion {
        version_id: version_label.into(),
        symbol: SYMBOL.into(),
        period: Period::Day,
        adjustment: Adjustment::Forward,
        source: "official".into(),
        records: bars,
        published_at: ts(),
        validation: ValidationSummary {
            record_count: 0,
            unique_record_count: 0,
            duplicate_count: 0,
        },
        unit_currency: units(),
        adjustment_evidence: AdjustmentEvidence::confirmed("qfq", "bound"),
        yearly_derivation: None,
    };
    let batch = MarketDataBatch {
        symbol: SYMBOL.into(),
        period: Period::Day,
        adjustment: Adjustment::Forward,
        bars: version.records.clone(),
        provenance: Provenance {
            source: "official".into(),
            source_period: "day".into(),
            source_adjustment: "qfq".into(),
            symbol: SYMBOL.into(),
            acquired_at: version.published_at,
            adapter_version: "effective-publish".into(),
            yearly_derivation: None,
            snapshot_inputs: None,
        },
        coverage: CoverageEvidence::complete("full history verified"),
        unit_currency: units(),
        adjustment_evidence: AdjustmentEvidence::confirmed("qfq", "bound"),
    };
    let (records, validation) = crate::publish::validate_and_deduplicate(&batch).unwrap();
    version.records = records;
    version.validation = validation;
    let mut raw = serde_json::to_value(&version).unwrap();
    raw["batch"] = serde_json::to_value(&batch).unwrap();
    raw["compact_source"] = json!({
        "generation_id": "gen-1",
        "generation_manifest_sha256": "0b".repeat(32),
        "day_member_sha256": "0c".repeat(32),
        "security_event_sha256": "0e".repeat(32),
        "generation_evidence": generation_evidence(),
    });
    raw["integrity_sha256"] = json!(compat::legacy_digest(&raw).unwrap());
    repo.stage(&raw).unwrap();
    let candidate = repo.candidate(version_label).unwrap();
    repo.mark_validated(&candidate).unwrap();
    let mut command = command(task_id, publication_id, revision);
    command.version_id = version_label.into();
    let plan = PublishPlan {
        command,
        record: candidate.clone(),
        required_heads: vec![(scope(), None)],
        promote: Some(scope()),
        base_digest: None,
    };
    repo.commit(&plan).unwrap()
}

#[test]
fn first_publish_creates_series_and_replays() {
    let dir = tempdir();
    let (path, repo) = repo(&dir);
    let (task_id, revision) = running_task(&repo);

    let receipt = publish_bars(
        &repo,
        &task_id,
        revision,
        "pub-1",
        "v1",
        vec![bar(0, 100), bar(1, 110)],
    );

    let c = Connection::open(&path).unwrap();
    let row = series::read_series(&c, SYMBOL, Period::Day)
        .unwrap()
        .unwrap();
    assert_eq!(receipt.version_id, row.version_id);
    assert_eq!(row.record_count, 2);
    // Event row: first_publish, old_digest NULL, replay anchor.
    let event = series::event_by_result(&c, "pub-1").unwrap().unwrap();
    assert_eq!(event.kind, "first_publish");
    assert!(event.old_content_digest.is_none());
    assert_eq!(event.new_content_digest, row.content_digest);
    // Replay under the same result id returns the same receipt.
    let replay = publish_bars(
        &repo,
        &task_id,
        revision,
        "pub-1",
        "v1",
        vec![bar(0, 100), bar(1, 110)],
    );
    assert_eq!(replay.version_id, receipt.version_id);
}

#[test]
fn append_then_replace_fold_equivalence() {
    // §8.1-1/2: two sequential appends then a full replace of the same
    // content produce the same digest; version_id tracks the digest.
    let dir = tempdir();
    let (path, repo) = repo(&dir);
    let (task_id, _r) = running_task_multi(&repo, 3);
    let rev1 = start_item_id(&repo, &task_id, "item-1");
    let first = publish_bars(
        &repo,
        &task_id,
        rev1,
        "pub-1",
        "v1",
        vec![bar(0, 100), bar(1, 110)],
    );

    // Append day 2 via the ops path.
    let rev2 = start_item_id(&repo, &task_id, "item-2");
    let upserts = vec![bar(2, 120)];
    let expected = vec![bar(0, 100), bar(1, 110), bar(2, 120)];
    let receipt2 = repo
        .commit_range_ops(
            &command_at(&task_id, "pub-2", rev2, "item-2"),
            &scope(),
            &first.version_id,
            &upserts,
            &[],
            &expected,
        )
        .unwrap()
        .unwrap();
    let c = Connection::open(&path).unwrap();
    let row = series::read_series(&c, SYMBOL, Period::Day)
        .unwrap()
        .unwrap();
    assert_eq!(row.version_id, receipt2.version_id);
    assert_eq!(row.record_count, 3);
    // digest after append == fresh fold over all bars.
    let all = series::all_bars(&c, row.series_key).unwrap();
    let refold = series::fold_bars(SYMBOL, Period::Day, &all).unwrap();
    assert_eq!(refold, row.content_digest);

    // §8.1-6: replace — old bars replaced wholesale; event continuity=3.
    let rev3 = start_item_id(&repo, &task_id, "item-3");
    let new_content = vec![bar(0, 200), bar(1, 210), bar(2, 220), bar(3, 230)];
    let receipt3 = repo
        .commit_range_ops(
            &command_at(&task_id, "pub-3", rev3, "item-3"),
            &scope(),
            &row.version_id,
            &[bar(0, 200)],
            &[date(1), date(2)],
            &new_content,
        )
        .unwrap()
        .unwrap();
    let c2 = Connection::open(&path).unwrap();
    let row2 = series::read_series(&c2, SYMBOL, Period::Day)
        .unwrap()
        .unwrap();
    assert_eq!(row2.version_id, receipt3.version_id);
    assert_eq!(row2.record_count, 4);
    let bars = series::all_bars(&c2, row2.series_key).unwrap();
    // Old close values are gone; new digest = fold over the new content.
    assert_eq!(bars[0].close_cents, 200);
    assert_eq!(
        series::fold_bars(SYMBOL, Period::Day, &bars).unwrap(),
        row2.content_digest
    );
    let event = series::event_by_result(&c2, "pub-3").unwrap().unwrap();
    assert_eq!(event.kind, "replace");
    assert_eq!(event.continuity_mode, Some(3));
    assert!(event.prefix_bytes.is_none());
    assert_eq!(event.old_content_digest, Some(row.content_digest));
}

// The running_task fixture above starts a one-item task; multi-item tests
// need the task to carry multiple items at acceptance. Rebuild it here.
fn running_task_multi(
    repo: &super::super::compact_repo::SqliteCompactMarket,
    count: usize,
) -> (String, u64) {
    use crate::task_control::TaskRepository;
    let scope = task_control::legacy::scope_id(SYMBOL, "day");
    let items: Vec<ItemSpec> = (1..=count)
        .map(|i| ItemSpec {
            item_id: format!("item-{i}"),
            target: json!({"symbol": SYMBOL, "period": "day"}),
            progress_unit: "bars".into(),
            frozen_inputs: json!({}),
            scope_keys: BTreeSet::from([scope.clone()]),
        })
        .collect();
    let work = task_control::FrozenWork {
        items,
        scopes: BTreeSet::from([scope]),
        shared_evidence: None,
    };
    let command = task_control::Command {
        key: CommandKey {
            operation: "update_data".into(),
            dataset: "market.daily".into(),
            contract_version: "v1".into(),
        },
        intent: json!({}),
    };
    let acceptance = repo.accept("req-m", &command, &work, RUN, None).unwrap();
    let task_id = match acceptance.record.outcome {
        task_control::RequestOutcome::Task(snapshot) => snapshot.task.task_id.clone(),
        _ => panic!("expected task outcome"),
    };
    let revision = repo.task(&task_id).unwrap().task.state_revision;
    let revision = repo
        .transition(&task_id, revision, RUN, task_control::Change::Claim)
        .unwrap()
        .task
        .state_revision;
    (task_id, revision)
}

fn start_item_id(
    repo: &super::super::compact_repo::SqliteCompactMarket,
    task_id: &str,
    item: &str,
) -> u64 {
    use crate::task_control::TaskRepository;
    let revision = repo.task(task_id).unwrap().task.state_revision;
    repo.transition(
        task_id,
        revision,
        RUN,
        task_control::Change::StartItem(item.into()),
    )
    .unwrap()
    .task
    .state_revision
}

#[test]
fn append_duplicate_day_fails_closed() {
    // §8.1-5: a raw INSERT duplicating an existing (series_key, trade_day)
    // violates the PK — and the typed ops path refuses a non-append diff.
    let dir = tempdir();
    let (path, repo) = repo(&dir);
    let (task_id, _revision) = running_task_multi(&repo, 2);
    let rev1 = start_item_id(&repo, &task_id, "item-1");
    let first = publish_bars(
        &repo,
        &task_id,
        rev1,
        "pub-1",
        "v1",
        vec![bar(0, 100), bar(1, 110)],
    );

    // Raw duplicate-day INSERT on the live series fails on the PK.
    let c = Connection::open(&path).unwrap();
    let key = series::read_series(&c, SYMBOL, Period::Day)
        .unwrap()
        .unwrap()
        .series_key;
    let dup = bar(1, 999);
    let err = series::insert_bars(&c, key, &[dup]).unwrap_err();
    assert!(
        matches!(err, Error::Integrity(_) | Error::Storage(_)),
        "{err:?}"
    );

    // Typed ops path: a non-tail upsert must fail closed (never rewrite).
    let rev2 = start_item_id(&repo, &task_id, "item-2");
    let err = repo
        .commit_range_ops_typed(
            &command_at(&task_id, "pub-2", rev2, "item-2"),
            &scope(),
            &first.version_id,
            &[bar(1, 999)], // collides with an existing day
            &[],
            None,
            None,
        )
        .unwrap_err();
    assert!(
        matches!(err, Error::Conflict(_) | Error::Stale(_)),
        "{err:?}"
    );

    // Series unchanged.
    repo.read(|c| {
        let row = series::read_series(c, SYMBOL, Period::Day)
            .unwrap()
            .unwrap();
        assert_eq!(row.version_id, first.version_id);
        assert_eq!(row.record_count, 2);
        Ok(())
    })
    .unwrap();
}

fn command_at(task_id: &str, publication_id: &str, revision: u64, item: &str) -> Publication {
    Publication {
        publication_id: publication_id.into(),
        version_id: "candidate-label".into(),
        task_id: task_id.into(),
        item_id: item.into(),
        run_id: RUN.into(),
        expected_revision: revision,
        intent: PublishIntent::Daily {
            expected_head: None,
        },
    }
}

#[test]
fn no_change_replays_idempotently() {
    // §8.1-16 + §8.2: same result_id twice → one event, identical receipt.
    let dir = tempdir();
    let (path, repo) = repo(&dir);
    let (task_id, _revision) = running_task_multi(&repo, 2);
    let rev1 = start_item_id(&repo, &task_id, "item-1");
    let first = publish_bars(
        &repo,
        &task_id,
        rev1,
        "pub-1",
        "v1",
        vec![bar(0, 100), bar(1, 110)],
    );

    let rev2 = start_item_id(&repo, &task_id, "item-2");
    let range = crate::market_update::DateRange {
        start: date(0),
        end: date(1),
    };
    let cmd = command_at(&task_id, "pub-2", rev2, "item-2");
    let r1 = repo
        .commit_range_no_change_typed(&cmd, &first.version_id, &range, None)
        .unwrap()
        .unwrap();
    let r2 = repo
        .commit_range_no_change_typed(&cmd, &first.version_id, &range, None)
        .unwrap()
        .unwrap();
    assert_eq!(r1, r2);
    let c = Connection::open(&path).unwrap();
    let events: i64 = c
        .query_row(
            "SELECT count(*) FROM market_events WHERE result_id='pub-2'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(events, 1);
    // Head unchanged; the event is a no_change outcome row.
    let row = series::read_series(&c, SYMBOL, Period::Day)
        .unwrap()
        .unwrap();
    assert_eq!(row.version_id, first.version_id);
    let event = series::event_by_result(&c, "pub-2").unwrap().unwrap();
    assert_eq!(event.kind, "no_change");
    assert_eq!(event.outcome, 1);
}

#[test]
fn stale_base_version_fails_closed() {
    // §8.2-14 shaped as the repo-level CAS: commit against a superseded
    // head must Stale-conflict, never silently rebase.
    let dir = tempdir();
    let (_path, repo) = repo(&dir);
    let (task_id, _r) = running_task_multi(&repo, 3);
    let rev1 = start_item_id(&repo, &task_id, "item-1");
    let first = publish_bars(&repo, &task_id, rev1, "pub-1", "v1", vec![bar(0, 100)]);

    let rev2 = start_item_id(&repo, &task_id, "item-2");
    repo.commit_range_ops(
        &command_at(&task_id, "pub-2", rev2, "item-2"),
        &scope(),
        &first.version_id,
        &[bar(1, 110)],
        &[],
        &[bar(0, 100), bar(1, 110)],
    )
    .unwrap()
    .unwrap();

    // A second commit pinned to the OLD head must fail — stale, not silent.
    let rev3 = start_item_id(&repo, &task_id, "item-3");
    let err = repo
        .commit_range_ops(
            &command_at(&task_id, "pub-3", rev3, "item-3"),
            &scope(),
            &first.version_id,
            &[bar(2, 120)],
            &[],
            &[bar(0, 100), bar(1, 110), bar(2, 120)],
        )
        .unwrap_err();
    assert!(matches!(err, Error::Stale(_)), "{err:?}");

    // And reads pinned to the superseded digest fail closed too.
    repo.read(|c| {
        assert!(matches!(
            series::require_series(c, &first.version_id),
            Err(Error::Stale(_))
        ));
        Ok(())
    })
    .unwrap();
}

#[test]
fn replace_rollback_keeps_series_consistent() {
    // §8.2-12: abort inside the commit transaction rolls everything back —
    // the series still points at the old digest and a retry succeeds.
    let dir = tempdir();
    let (_path, repo) = repo(&dir);
    let (task_id, _r) = running_task_multi(&repo, 3);
    let rev1 = start_item_id(&repo, &task_id, "item-1");
    let first = publish_bars(&repo, &task_id, rev1, "pub-1", "v1", vec![bar(0, 100)]);

    // Drive commit_series_tx inside a manual BEGIN and roll it back —
    // the same failure shape as a kill between DELETE and INSERT.
    let rev2 = start_item_id(&repo, &task_id, "item-2");
    let cmd = command_at(&task_id, "pub-2", rev2, "item-2");
    let new_bars = to_compact_bars(&[bar(0, 200), bar(1, 210)], Period::Day).unwrap();
    repo.write(|c| {
        let tx = c
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(storage)?;
        let (ev_d, bind_d) = put_seed_material(&tx);
        let continuity = put_continuity_evidence(&tx, &json!({"mode":"full"}))?;
        let result = commit_series_tx(
            &tx,
            &Commit {
                result_id: &cmd.publication_id,
                task_id: &cmd.task_id,
                item_id: &cmd.item_id,
                run_id: &cmd.run_id,
                occupancy_key: &scope().occupancy_key(),
                expected_task_revision: rev2,
                entity: SYMBOL,
                period: Period::Day,
                base_version_id: Some(&first.version_id),
                change: SeriesChange::Replace { bars: &new_bars },
                range_start_day: 0,
                range_end_day: 1,
                algorithm_version: UPDATE_ALGO,
                window_digest: None,
                source_binding_digest: Some(bind_d),
                evidence_digest: Some(ev_d),
                coverage_status: Some(CoverageStatus::Complete),
                metadata_changed: false,
                event_evidence_digest: continuity,
                published_at: None,
            },
        )?;
        let _ = result;
        drop(tx); // rollback — never committed
        Ok::<_, Error>(())
    })
    .unwrap();

    // State intact: head still the first publish's digest.
    repo.read(|c| {
        let row = series::read_series(c, SYMBOL, Period::Day)
            .unwrap()
            .unwrap();
        assert_eq!(row.version_id, first.version_id);
        assert_eq!(row.record_count, 1);
        // No orphaned event row either.
        assert!(series::event_by_result(c, "pub-2").unwrap().is_none());
        Ok(())
    })
    .unwrap();

    // Retry the same result id commits cleanly.
    let receipt = repo
        .commit_range_ops(
            &command_at(&task_id, "pub-3", rev2, "item-2"),
            &scope(),
            &first.version_id,
            &[bar(1, 110)],
            &[],
            &[bar(0, 100), bar(1, 110)],
        )
        .unwrap()
        .unwrap();
    assert_ne!(receipt.version_id, first.version_id);
}

#[test]
fn missing_tail_event_fails_startup_check() {
    // §8.2-13: deleting the newest event row for a series must trip the
    // tail-closure probe (last_event_seq no longer names an existing event).
    let dir = tempdir();
    let (path, repo) = repo(&dir);
    let (task_id, revision) = running_task(&repo);
    publish_bars(&repo, &task_id, revision, "pub-1", "v1", vec![bar(0, 100)]);

    let c = Connection::open(&path).unwrap();
    // Bypass the commit machinery: raw SQL delete of the tail event.
    c.execute(
        "DELETE FROM market_events WHERE seq=(SELECT max(seq) FROM market_events)",
        [],
    )
    .unwrap();
    assert!(series::startup_check(&c).is_err());
}

#[test]
fn read_snapshot_isolated_from_concurrent_append() {
    // §8.2-15: a long-running read transaction keeps its snapshot while a
    // concurrent append commits (WAL semantics regression).
    let dir = tempdir();
    let (path, repo) = repo(&dir);
    let (task_id, _r) = running_task_multi(&repo, 2);
    let rev1 = start_item_id(&repo, &task_id, "item-1");
    let first = publish_bars(&repo, &task_id, rev1, "pub-1", "v1", vec![bar(0, 100)]);

    // Read snapshot on a separate connection opened BEFORE the append.
    let mut reader = Connection::open(&path).unwrap();
    let rtx = reader.transaction().unwrap();
    let before = series::bars_in_range(
        &rtx,
        series::read_series(&rtx, SYMBOL, Period::Day)
            .unwrap()
            .unwrap()
            .series_key,
        0,
        10,
    )
    .unwrap();
    assert_eq!(before.len(), 1);

    let rev2 = start_item_id(&repo, &task_id, "item-2");
    repo.commit_range_ops(
        &command_at(&task_id, "pub-2", rev2, "item-2"),
        &scope(),
        &first.version_id,
        &[bar(1, 110)],
        &[],
        &[bar(0, 100), bar(1, 110)],
    )
    .unwrap()
    .unwrap();

    // The open read transaction still sees the old bars.
    let after = series::bars_in_range(
        &rtx,
        series::read_series(&rtx, SYMBOL, Period::Day)
            .unwrap()
            .unwrap()
            .series_key,
        0,
        10,
    )
    .unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].close_cents, 100);
    drop(rtx);
}
