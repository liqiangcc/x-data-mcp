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
    let dup = crate::compact_market::CompactBar {
        trade_day: 1,
        open_cents: 999,
        high_cents: 999,
        low_cents: 999,
        close_cents: 999,
        volume: Some(1),
        amount: Some("1".into()),
        r7_raw: Some(7),
    };
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

// ---------------------------------------------------------------------------
// F4 coverage ports (S2b): behaviors that lost coverage when the v19 test
// modules were retired. Names mirror the deleted tests where applicable.
// ---------------------------------------------------------------------------

fn metadata_gen(generation: &str, manifest: &str) -> crate::market_update::TypedVersionMetadata {
    crate::market_update::TypedVersionMetadata {
        generation_id: generation.into(),
        generation_manifest_sha256: manifest.into(),
        day_member_sha256: "12".repeat(32),
        security_event_sha256: "ef".repeat(32),
        generation_evidence: json!({
            "manifest_sha256": manifest,
            "snapshot_inputs": [
                {"role": "day", "member": "sh/lday/sh600006.day",
                 "archive_sha256": "34".repeat(32), "member_sha256": "12".repeat(32)},
                {"role": "gbbq-security-events", "member": "gbbq#sh.600006",
                 "archive_sha256": "56".repeat(32), "member_sha256": "ef".repeat(32)}
            ],
        }),
        source: "official".into(),
        coverage: CoverageEvidence::complete("complete .day history from generation B"),
        unit_currency: units(),
        adjustment_evidence: AdjustmentEvidence::confirmed("qfq", "bound"),
    }
}

#[test]
fn typed_append_rebinds_generation_metadata_and_rejects_drift() {
    // P1: typed append rebinds to the carried generation and rejects
    // source/adjustment/unit drift and non-complete coverage.
    let dir = tempdir();
    let (path, repo) = repo(&dir);
    let (task_id, _r) = running_task_multi(&repo, 6);
    let rev1 = start_item_id(&repo, &task_id, "item-1");
    let first = publish_bars(
        &repo,
        &task_id,
        rev1,
        "pub-1",
        "v1",
        vec![bar(0, 100), bar(1, 110), bar(2, 120)],
    );

    let metadata = metadata_gen("gen-b", &"be".repeat(32));
    let rev2 = start_item_id(&repo, &task_id, "item-2");
    let receipt = repo
        .commit_range_ops_typed(
            &command_at(&task_id, "pub-2", rev2, "item-2"),
            &scope(),
            &first.version_id,
            &[bar(3, 130)],
            &[],
            Some(&metadata),
            None,
        )
        .unwrap()
        .unwrap();
    let c = Connection::open(&path).unwrap();
    let bound: String = c
        .query_row(
            "SELECT sb.generation_id FROM market_series v \
             JOIN source_bindings sb ON sb.source_binding_digest=v.source_binding_digest \
             WHERE v.version_id=?1",
            [receipt.version_id.clone()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(bound, "gen-b", "typed metadata append must rebind");
    let row = series::read_series(&c, SYMBOL, Period::Day)
        .unwrap()
        .unwrap();
    assert_eq!(row.record_count, 4);

    // Source drift.
    let mut bad_source = metadata.clone();
    bad_source.source = "mirror".into();
    let rev3 = start_item_id(&repo, &task_id, "item-3");
    let err = repo
        .commit_range_ops_typed(
            &command_at(&task_id, "pub-3", rev3, "item-3"),
            &scope(),
            &receipt.version_id,
            &[bar(4, 140)],
            &[],
            Some(&bad_source),
            None,
        )
        .expect_err("source drift must be rejected");
    assert!(
        err.to_string().contains("drifts source or adjustment"),
        "unexpected rejection: {err}"
    );

    // Adjustment-evidence drift.
    let mut bad_adj = metadata.clone();
    bad_adj.adjustment_evidence = AdjustmentEvidence::confirmed("hfq", "drift");
    let rev4 = start_item_id(&repo, &task_id, "item-4");
    let err = repo
        .commit_range_ops_typed(
            &command_at(&task_id, "pub-4", rev4, "item-4"),
            &scope(),
            &receipt.version_id,
            &[bar(4, 140)],
            &[],
            Some(&bad_adj),
            None,
        )
        .expect_err("adjustment drift must be rejected");
    assert!(
        err.to_string().contains("drifts source or adjustment"),
        "unexpected rejection: {err}"
    );

    // Unit drift.
    let mut bad_units = metadata.clone();
    bad_units.unit_currency.price_unit = EvidenceValue::confirmed("fen", "drift");
    let rev5 = start_item_id(&repo, &task_id, "item-5");
    let err = repo
        .commit_range_ops_typed(
            &command_at(&task_id, "pub-5", rev5, "item-5"),
            &scope(),
            &receipt.version_id,
            &[bar(4, 140)],
            &[],
            Some(&bad_units),
            None,
        )
        .expect_err("unit drift must be rejected");
    assert!(
        err.to_string().contains("drifts units"),
        "unexpected rejection: {err}"
    );

    // Non-complete coverage.
    let mut bad_cov = metadata.clone();
    bad_cov.coverage = CoverageEvidence {
        status: crate::market_data::CoverageStatus::Incomplete,
        reason: "partial".into(),
    };
    let rev6 = start_item_id(&repo, &task_id, "item-6");
    let err = repo
        .commit_range_ops_typed(
            &command_at(&task_id, "pub-6", rev6, "item-6"),
            &scope(),
            &receipt.version_id,
            &[bar(4, 140)],
            &[],
            Some(&bad_cov),
            None,
        )
        .expect_err("non-complete coverage must be rejected");
    assert!(
        err.to_string().contains("coverage is not complete"),
        "unexpected rejection: {err}"
    );

    // The series still shows the 4-bar append only.
    let row = series::read_series(&c, SYMBOL, Period::Day)
        .unwrap()
        .unwrap();
    assert_eq!(row.record_count, 4);
    assert_eq!(row.version_id, receipt.version_id);
}

#[test]
fn collection_rejects_unconfirmed_unit_evidence_and_existing_series() {
    // F1: the unit-evidence gate must fail the offending item inside the
    // batch and must not touch the other items' series state.
    use crate::official_market::{CollectionCandidate, DailyCollectionRepository};
    let dir = tempdir();
    let (_path, repo) = repo(&dir);
    let (task_id, _r) = running_task_multi(&repo, 4);

    fn candidate(task_id: &str, item: &str, units: UnitCurrencyEvidence) -> CollectionCandidate {
        CollectionCandidate {
            task_id: task_id.into(),
            item_id: item.into(),
            publication_id: format!("col-{item}"),
            version_id: format!("v-{item}"),
            run_id: RUN.into(),
            symbol: SYMBOL.into(),
            source: "official".into(),
            bars: vec![bar(0, 100), bar(1, 110)],
            coverage: CoverageEvidence::complete("full history verified"),
            unit_currency: units,
            adjustment_evidence: AdjustmentEvidence::confirmed("qfq", "bound"),
            published_at: ts(),
            generation_id: "gen-1".into(),
            generation_manifest_sha256: "0b".repeat(32),
            day_member_sha256: "0c".repeat(32),
            security_event_sha256: "0e".repeat(32),
            generation_evidence: generation_evidence(),
        }
    }

    let mut no_price = units();
    no_price.price_unit = EvidenceValue::unknown("unconfirmed");
    let mut no_volume = units();
    no_volume.volume_unit = EvidenceValue::unknown("unconfirmed");
    let mut no_amount = units();
    no_amount.amount_unit = EvidenceValue::unknown("unconfirmed");

    let items = vec![
        candidate(&task_id, "item-1", units()),
        candidate(&task_id, "item-2", no_price),
        candidate(&task_id, "item-3", no_volume),
        candidate(&task_id, "item-4", no_amount),
    ];
    let out = repo
        .commit_collection_batch(&task_id, &items)
        .unwrap()
        .expect("typed collection batch supported");
    assert_eq!(out.len(), 4);
    assert!(out[0].is_ok(), "first item must commit: {:?}", out[0]);
    for (i, res) in out.iter().enumerate().skip(1) {
        let err = res.as_ref().expect_err(&format!("item {i} must fail"));
        assert!(
            err.to_string().contains("unit"),
            "item {i} unexpected rejection: {err}"
        );
    }
    // One bad item did not roll back the committed one: the committed
    // series row exists with its bars.
    repo.read(|c| {
        let row = series::read_series(c, SYMBOL, Period::Day)
            .unwrap()
            .unwrap();
        assert_eq!(row.record_count, 2);
        Ok(())
    })
    .unwrap();
    // Item failure recorded: only one item reached Finished.
    let snapshot = repo.task(&task_id).unwrap();
    let finished = snapshot
        .items
        .iter()
        .filter(|i| i.stage == task_control::Stage::Finished)
        .count();
    assert_eq!(finished, 4, "every item must carry a terminal outcome");

    // Collection on an existing series -> Conflict (drift checks do not
    // apply to first-publish semantics).
    let (task2, _r2) = running_task(&repo);
    let single = vec![candidate(&task2, ITEM, units())];
    let out2 = repo
        .commit_collection_batch(&task2, &single)
        .unwrap()
        .unwrap();
    let err = out2[0].as_ref().expect_err("second publish must conflict");
    assert!(
        matches!(err, Error::Conflict(_)),
        "expected Conflict, got {err:?}"
    );
}

#[test]
fn update_batch_commit_mixed_isolation_and_rollback() {
    // P2: SAVEPOINT isolation — a failing item rolls back alone, the rest
    // commit, and the task counters stay consistent.
    use crate::market_update::{UpdateBatchItem, UpdateBatchPrepared};
    let dir = tempdir();
    let (_path, repo) = repo(&dir);
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

    let items = vec![
        UpdateBatchItem {
            task_id: task_id.clone(),
            item_id: "item-2".into(),
            publication_id: "upd-ok".into(),
            run_id: RUN.into(),
            symbol: SYMBOL.into(),
            occupancy_key: scope().occupancy_key(),
            prepared: UpdateBatchPrepared::Append {
                base_version_id: first.version_id.clone(),
                result_version_id: "v2".into(),
                upserts: vec![bar(2, 120)],
                metadata: None,
                continuity: json!({"proven": true}),
            },
        },
        UpdateBatchItem {
            task_id: task_id.clone(),
            item_id: "item-3".into(),
            publication_id: "upd-bad".into(),
            run_id: RUN.into(),
            symbol: SYMBOL.into(),
            occupancy_key: scope().occupancy_key(),
            // Stale base: the head already moved past this version.
            prepared: UpdateBatchPrepared::Append {
                base_version_id: "0".repeat(64),
                result_version_id: "v3".into(),
                upserts: vec![bar(3, 130)],
                metadata: None,
                continuity: json!({"proven": true}),
            },
        },
    ];
    let out = repo.commit_update_batch(&items).unwrap().unwrap();
    assert_eq!(out.len(), 2);
    let ok = out[0].as_ref().expect("first item must commit");
    assert_eq!(ok.publication_id, "upd-ok");
    let err = out[1].as_ref().expect_err("stale-base item must fail");
    assert!(
        matches!(err, Error::NotFound | Error::Stale(_) | Error::Conflict(_)),
        "unexpected failure kind: {err:?}"
    );
    repo.read(|c| {
        let row = series::read_series(c, SYMBOL, Period::Day)
            .unwrap()
            .unwrap();
        assert_eq!(row.record_count, 3, "committed item's append must stick");
        Ok(())
    })
    .unwrap();
}

#[test]
fn typed_and_envelope_paths_store_identical_series_bytes() {
    // P3: same bars + generation + evidence committed through the
    // envelope path and the typed collection batch land byte-identical
    // series state (content_digest / version_id / bar rows).
    use crate::official_market::{CollectionCandidate, DailyCollectionRepository};
    let dir_a = tempdir();
    let (_p_a, repo_a) = repo(&dir_a);
    let (task_a, rev_a) = running_task(&repo_a);
    let env = publish_bars(
        &repo_a,
        &task_a,
        rev_a,
        "pub-env",
        "v1",
        vec![bar(0, 100), bar(1, 110), bar(2, 120)],
    );

    let dir_b = tempdir();
    let (_p_b, repo_b) = repo(&dir_b);
    let (task_b, _r_b) = running_task(&repo_b);
    let item = CollectionCandidate {
        task_id: task_b.clone(),
        item_id: ITEM.into(),
        publication_id: "pub-typed".into(),
        version_id: "v1".into(),
        run_id: RUN.into(),
        symbol: SYMBOL.into(),
        source: "official".into(),
        bars: vec![bar(0, 100), bar(1, 110), bar(2, 120)],
        coverage: CoverageEvidence::complete("full history verified"),
        unit_currency: units(),
        adjustment_evidence: AdjustmentEvidence::confirmed("qfq", "bound"),
        published_at: ts(),
        generation_id: "gen-1".into(),
        generation_manifest_sha256: "0b".repeat(32),
        day_member_sha256: "0c".repeat(32),
        security_event_sha256: "0e".repeat(32),
        generation_evidence: generation_evidence(),
    };
    let out = repo_b
        .commit_collection_batch(&task_b, &[item])
        .unwrap()
        .unwrap();
    let typed = out[0].as_ref().expect("typed collection must commit");
    assert_eq!(
        typed.version_id, env.version_id,
        "same input -> same digest"
    );

    let c_a = repo_a
        .read(|c| {
            let row = series::read_series(c, SYMBOL, Period::Day)
                .unwrap()
                .unwrap();
            Ok((
                row.content_digest,
                series::all_bars(c, row.series_key).unwrap(),
            ))
        })
        .unwrap();
    let c_b = repo_b
        .read(|c| {
            let row = series::read_series(c, SYMBOL, Period::Day)
                .unwrap()
                .unwrap();
            Ok((
                row.content_digest,
                series::all_bars(c, row.series_key).unwrap(),
            ))
        })
        .unwrap();
    assert_eq!(c_a, c_b, "stored digest and bar rows must match");
}

#[test]
fn ops_result_id_replay_identity_conflict_and_no_change_purity() {
    // P4: result_id replay returns the same receipt; same result_id under
    // a different task/item is a Conflict; a no_change commit writes an
    // event row and leaves bars untouched.
    let dir = tempdir();
    let (path, repo) = repo(&dir);
    let (task_id, _r) = running_task_multi(&repo, 4);
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
    let append = repo
        .commit_range_ops(
            &command_at(&task_id, "pub-2", rev2, "item-2"),
            &scope(),
            &first.version_id,
            &[bar(2, 120)],
            &[],
            &[bar(0, 100), bar(1, 110), bar(2, 120)],
        )
        .unwrap()
        .unwrap();
    // Same command replays -> identical receipt, no new event.
    let replay = repo
        .commit_range_ops(
            &command_at(&task_id, "pub-2", rev2, "item-2"),
            &scope(),
            &first.version_id,
            &[bar(2, 120)],
            &[],
            &[bar(0, 100), bar(1, 110), bar(2, 120)],
        )
        .unwrap()
        .unwrap();
    assert_eq!(replay.version_id, append.version_id);
    let c = Connection::open(&path).unwrap();
    let events: i64 = c
        .query_row(
            "SELECT count(*) FROM market_events WHERE result_id='pub-2'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(events, 1, "replay must not write a second event");

    // Same result_id under a different task -> Conflict.
    let (task2, _r2) = running_task_multi(&repo, 2);
    let rev_b = start_item_id(&repo, &task2, "item-1");
    let err = repo
        .commit_range_ops(
            &command_at(&task2, "pub-2", rev_b, "item-1"),
            &scope(),
            &append.version_id,
            &[bar(3, 130)],
            &[],
            &[bar(0, 100), bar(1, 110), bar(2, 120), bar(3, 130)],
        )
        .expect_err("same result id on a different task must conflict");
    assert!(
        matches!(err, Error::Conflict(_)),
        "expected Conflict, got {err:?}"
    );

    // no_change: event row written, bars untouched.
    let rev3 = start_item_id(&repo, &task_id, "item-3");
    let range = crate::market_update::DateRange {
        start: date(0),
        end: date(2),
    };
    let receipt = repo
        .commit_range_no_change_typed(
            &command_at(&task_id, "pub-nc", rev3, "item-3"),
            &append.version_id,
            &range,
            None,
        )
        .unwrap()
        .unwrap();
    assert_eq!(receipt.version_id, append.version_id);
    let event = series::event_by_result(&c, "pub-nc").unwrap().unwrap();
    assert_eq!(event.kind, "no_change");
    let row = series::read_series(&c, SYMBOL, Period::Day)
        .unwrap()
        .unwrap();
    assert_eq!(row.record_count, 3);
    assert_eq!(row.last_day, 2);
    let bars = series::all_bars(&c, row.series_key).unwrap();
    assert_eq!(bars.len(), 3);
}

#[test]
fn check_binding_rejects_wrong_owner_state_target_and_occupancy() {
    // P5: check_binding's four rejections, driven through the real commit
    // gate with a directly constructed Commit.
    let dir = tempdir();
    let (_path, repo) = repo(&dir);
    let (task_id, _r) = running_task(&repo);

    let base_commit = |task: &str, item: &str, run: &str, symbol: &str| Commit {
        result_id: "bind-1",
        task_id: task,
        item_id: item,
        run_id: run,
        occupancy_key: "",
        expected_task_revision: 1,
        entity: symbol,
        period: Period::Day,
        base_version_id: None,
        change: SeriesChange::FirstPublish { bars: &[] },
        range_start_day: 0,
        range_end_day: 0,
        algorithm_version: "test",
        window_digest: None,
        source_binding_digest: None,
        evidence_digest: None,
        coverage_status: None,
        metadata_changed: false,
        event_evidence_digest: [0xAA; 32],
        published_at: None,
    };

    let occ = scope().occupancy_key();
    let real_revision = repo.task(&task_id).unwrap().task.state_revision;

    // Owner run mismatch: task claimed by RUN, commit claims "other".
    let mut w = base_commit(&task_id, ITEM, "other", SYMBOL);
    w.occupancy_key = &occ;
    let err = repo.write(|c| commit_series_tx(c, &w)).unwrap_err();
    assert!(matches!(err, Error::Conflict(_)), "owner mismatch: {err:?}");

    // Wrong item_id -> target/item mismatch inside the same commit gate.
    let mut w = base_commit(&task_id, "item-9", RUN, SYMBOL);
    w.occupancy_key = &occ;
    let err = repo.write(|c| commit_series_tx(c, &w)).unwrap_err();
    assert!(
        matches!(err, Error::Task(_) | Error::Conflict(_)),
        "wrong item: {err:?}"
    );

    // Target mismatch: commit entity differs from the item's target.
    let mut w = base_commit(&task_id, ITEM, RUN, "sh.600007");
    w.occupancy_key = &occ;
    let err = repo.write(|c| commit_series_tx(c, &w)).unwrap_err();
    assert!(
        matches!(err, Error::Conflict(_)),
        "target mismatch: {err:?}"
    );

    // Occupancy mismatch: scope key not in the item's bound scopes.
    let mut w = base_commit(&task_id, ITEM, RUN, SYMBOL);
    w.occupancy_key = "unrelated-scope";
    let err = repo.write(|c| commit_series_tx(c, &w)).unwrap_err();
    assert!(matches!(err, Error::Conflict(_)), "occupancy: {err:?}");

    // Correct binding commits.
    let mut w = base_commit(&task_id, ITEM, RUN, SYMBOL);
    w.occupancy_key = &occ;
    w.expected_task_revision = real_revision;
    w.change = SeriesChange::FirstPublish { bars: &[BAR0] };
    repo.write(|c| commit_series_tx(c, &w)).unwrap();
}

const BAR0: CompactBar = CompactBar {
    trade_day: 0,
    open_cents: 100,
    high_cents: 110,
    low_cents: 90,
    close_cents: 100,
    volume: Some(1),
    amount: None,
    r7_raw: None,
};

#[test]
fn typed_append_base_coverage_parity_with_envelope_bytes() {
    // P6: a typed metadata append that carries the base's own coverage
    // keeps stored coverage/evidence identical to the envelope shape.
    let dir = tempdir();
    let (path, repo) = repo(&dir);
    let (task_id, _r) = running_task_multi(&repo, 2);
    let rev1 = start_item_id(&repo, &task_id, "item-1");
    let first = publish_bars(
        &repo,
        &task_id,
        rev1,
        "pub-1",
        "v1",
        vec![bar(0, 100), bar(1, 110)],
    );

    // Coverage identical to the base's committed evidence.
    let mut metadata = metadata_gen("gen-b", &"be".repeat(32));
    metadata.coverage = CoverageEvidence::complete("full history verified");
    let rev2 = start_item_id(&repo, &task_id, "item-2");
    let receipt = repo
        .commit_range_ops_typed(
            &command_at(&task_id, "pub-2", rev2, "item-2"),
            &scope(),
            &first.version_id,
            &[bar(2, 120)],
            &[],
            Some(&metadata),
            None,
        )
        .unwrap()
        .unwrap();
    let c = Connection::open(&path).unwrap();
    let (cov, evidence_json): (i64, String) = c
        .query_row(
            "SELECT v.coverage_status, e.canonical_json FROM market_series v \
             JOIN evidence_bundles e ON e.evidence_digest=v.evidence_digest \
             WHERE v.version_id=?1",
            [receipt.version_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(cov, 0, "coverage must stay complete (code 0)");
    let ev: Value = serde_json::from_str(&evidence_json).unwrap();
    assert_eq!(ev["coverage_reason"], "full history verified");
    assert_eq!(ev["unit_currency"]["price_unit"]["value"], "share");
}

#[test]
fn second_commit_on_same_item_fails_closed() {
    // P7: once an item is finished, a second commit — even under a new
    // result id — fails at the binding gate; and a raw duplicate
    // (task_id,item_id) event INSERT trips the UNIQUE backstop.
    let dir = tempdir();
    let (path, repo) = repo(&dir);
    let (task_id, _r) = running_task(&repo);
    let rev = repo.task(&task_id).unwrap().task.state_revision;
    let first = publish_bars(&repo, &task_id, rev, "pub-1", "v1", vec![bar(0, 100)]);

    let cmd = Publication {
        publication_id: "pub-2".into(),
        version_id: "v2".into(),
        task_id: task_id.clone(),
        item_id: ITEM.into(),
        run_id: RUN.into(),
        expected_revision: rev,
        intent: PublishIntent::Daily {
            expected_head: Some(first.version_id.clone()),
        },
    };
    let err = repo
        .commit_range_ops(
            &cmd,
            &scope(),
            &first.version_id,
            &[bar(1, 110)],
            &[],
            &[bar(0, 100), bar(1, 110)],
        )
        .expect_err("finished item must reject a second result");
    assert!(
        matches!(err, Error::Conflict(_) | Error::Task(_)),
        "unexpected: {err:?}"
    );

    // UNIQUE(task_id,item_id) backstop at the storage layer: re-inserting
    // the same (task_id,item_id) pair under a different result id trips it.
    let c = Connection::open(&path).unwrap();
    let dup = c.execute(
        "INSERT INTO market_events(seq,entity,period,kind,outcome,metadata_changed,new_content_digest,evidence_digest,task_id,item_id,task_revision,result_id,result_digest,created_at) \
         SELECT (SELECT max(seq)+1 FROM market_events),entity,period,'append',outcome,metadata_changed,new_content_digest,evidence_digest,task_id,item_id,task_revision,'pub-x',result_digest,created_at \
         FROM market_events WHERE result_id='pub-1'",
        [],
    );
    assert!(dup.is_err(), "duplicate (task_id,item_id) must be rejected");
}

#[test]
fn replace_with_constraint_violating_bar_rolls_back_fully() {
    // P8: real mid-commit failure — the replace's DELETE succeeds, then
    // the INSERT of a CHECK-violating bar fails and the whole transaction
    // rolls back: old bars/digest/last_event_seq/events/item all intact,
    // and a legal replace afterwards still commits.
    let dir = tempdir();
    let (path, repo) = repo(&dir);
    let (task_id, _r) = running_task_multi(&repo, 2);
    let rev1 = start_item_id(&repo, &task_id, "item-1");
    let first = publish_bars(
        &repo,
        &task_id,
        rev1,
        "pub-1",
        "v1",
        vec![bar(0, 100), bar(1, 110)],
    );

    // Craft a bar that passes encode but violates the CHECK (high < low).
    let mut bad = bar(0, 100);
    bad.high = rust_decimal::Decimal::new(50, 2); // 0.50 < open/close/low
    bad.low = rust_decimal::Decimal::new(400, 2); // 4.00 > high
    let expected_bad = vec![bad, bar(1, 110)];
    let rev2 = start_item_id(&repo, &task_id, "item-2");
    let err = repo
        .commit_range_ops(
            &command_at(&task_id, "pub-2", rev2, "item-2"),
            &scope(),
            &first.version_id,
            &[],
            &[],
            &expected_bad,
        )
        .expect_err("CHECK-violating replace must fail");

    let c = Connection::open(&path).unwrap();
    let row = series::read_series(&c, SYMBOL, Period::Day)
        .unwrap()
        .unwrap();
    assert_eq!(row.version_id, first.version_id, "state row must be intact");
    assert_eq!(row.record_count, 2);
    let bars = series::all_bars(&c, row.series_key).unwrap();
    assert_eq!(bars.len(), 2, "old bars must survive the rollback");
    assert_eq!(bars[0].trade_day, 0);
    assert_eq!(bars[1].trade_day, 1);
    let events: i64 = c
        .query_row("SELECT count(*) FROM market_events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(events, 1, "no event may be written");
    let item_state: String = c
        .query_row(
            "SELECT stage FROM task_items WHERE task_id=?1 AND item_id='item-2'",
            [&task_id],
            |r| r.get(0),
        )
        .unwrap_or_else(|_| "missing".into());
    assert_ne!(item_state, "finished", "item must not record a result");

    // A legal replace afterwards commits normally.
    let rev3 = repo.task(&task_id).unwrap().task.state_revision;
    let good = repo
        .commit_range_ops(
            &command_at(&task_id, "pub-3", rev3, "item-2"),
            &scope(),
            &first.version_id,
            &[],
            &[],
            &[bar(0, 200), bar(1, 210), bar(2, 220)],
        )
        .unwrap()
        .unwrap();
    let row = series::read_series(&c, SYMBOL, Period::Day)
        .unwrap()
        .unwrap();
    assert_eq!(row.version_id, good.version_id);
    assert_eq!(row.record_count, 3);
    let _ = err;
}

#[test]
fn commit_consumes_the_staged_record() {
    // P9/F5: a successful envelope commit deletes its staged_records row.
    let dir = tempdir();
    let (path, repo) = repo(&dir);
    let (task_id, rev) = running_task(&repo);
    publish_bars(&repo, &task_id, rev, "pub-1", "v1", vec![bar(0, 100)]);
    let c = Connection::open(&path).unwrap();
    let staged: i64 = c
        .query_row(
            "SELECT count(*) FROM staged_records WHERE version_id='v1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(staged, 0, "committed candidate must leave no staged row");
}

#[test]
fn metadata_commit_updates_updated_at_no_change_does_not() {
    // F5: a metadata event bumps market_series.updated_at; a no_change
    // event leaves it untouched.
    let dir = tempdir();
    let (path, repo) = repo(&dir);
    let (task_id, _r) = running_task_multi(&repo, 3);
    let rev1 = start_item_id(&repo, &task_id, "item-1");
    let first = publish_bars(&repo, &task_id, rev1, "pub-1", "v1", vec![bar(0, 100)]);
    let c = Connection::open(&path).unwrap();
    let created: String = c
        .query_row(
            "SELECT updated_at FROM market_series WHERE version_id=?1",
            [&first.version_id],
            |r| r.get(0),
        )
        .unwrap();

    // no_change: updated_at unchanged.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let rev2 = start_item_id(&repo, &task_id, "item-2");
    let range = crate::market_update::DateRange {
        start: date(0),
        end: date(0),
    };
    repo.commit_range_no_change_typed(
        &command_at(&task_id, "pub-nc", rev2, "item-2"),
        &first.version_id,
        &range,
        None,
    )
    .unwrap();
    let after_nc: String = c
        .query_row(
            "SELECT updated_at FROM market_series WHERE version_id=?1",
            [&first.version_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(after_nc, created, "no_change must not touch updated_at");

    // metadata: updated_at advances (SeriesChange::Metadata driven
    // directly — the ops path cannot emit a metadata-only change).
    let rev3 = start_item_id(&repo, &task_id, "item-3");
    repo.write(|c| {
        let binding = put_generation_binding(
            c,
            "gen-b",
            &"be".repeat(32),
            &"12".repeat(32),
            &"ef".repeat(32),
            &generation_evidence(),
        )?;
        let evidence = evidence_store::put_evidence(c, EvidenceKind::Version, &version_evidence())?;
        let head = series::read_series(c, SYMBOL, Period::Day)?.unwrap();
        let occupancy = scope().occupancy_key();
        commit_series_tx(
            c,
            &Commit {
                result_id: "pub-md",
                task_id: &task_id,
                item_id: "item-3",
                run_id: RUN,
                occupancy_key: &occupancy,
                expected_task_revision: rev3,
                entity: SYMBOL,
                period: Period::Day,
                base_version_id: Some(first.version_id.as_str()),
                change: SeriesChange::Metadata,
                range_start_day: head.first_day,
                range_end_day: head.last_day,
                algorithm_version: "test",
                window_digest: None,
                source_binding_digest: Some(binding),
                evidence_digest: Some(evidence),
                coverage_status: Some(crate::market_data::CoverageStatus::Complete),
                metadata_changed: true,
                event_evidence_digest: evidence,
                published_at: None,
            },
        )
    })
    .unwrap();
    let after_md: String = c
        .query_row(
            "SELECT updated_at FROM market_series WHERE version_id=?1",
            [&first.version_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_ne!(after_md, created, "metadata must bump updated_at");
}

#[test]
fn hex_digest_rejects_malformed_input() {
    // F2: the hex parser must fail closed, never panic on odd length,
    // non-hex, non-ASCII, or off-by-one lengths.
    for bad in [
        "abc".to_string(), // odd length
        "zz".repeat(32),   // non-hex chars
        "é".repeat(64),    // non-ASCII multibyte
        "0".repeat(63),    // 63 chars
        "0".repeat(65),    // 65 chars
        "0".repeat(128),   // way too long
    ] {
        assert!(hex_digest(&bad).is_err(), "accepted {bad:?}");
    }
    assert!(hex_digest(&"0".repeat(64)).is_ok());
    assert!(hex_digest(&"Ab".repeat(32)).is_ok());
}
