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
use crate::task_control::{self, CommandKey, ItemSpec, TaskRepository};
use crate::yearly_publication::results::YearlyRepository;
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
        open_units: cents,
        high_units: cents + 10,
        low_units: cents - 10,
        close_units: cents,
        volume: Some(100),
        amount: Some("1000".into()),
        r7_raw: Some(7),
    }
    .to_bar(Period::Day, crate::compact_market::PRICE_SCALE_CENTS)
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
    running_task_for(repo, "req-1", ITEM)
}

/// `running_task` with an explicit request/item id, so one test can commit
/// two daily items (two tasks) over the same scope.
fn running_task_for(
    repo: &super::super::compact_repo::SqliteCompactMarket,
    request: &str,
    item: &str,
) -> (String, u64) {
    use crate::task_control::TaskRepository;
    let scope = task_control::legacy::scope_id(SYMBOL, "day");
    let work = task_control::FrozenWork {
        items: vec![ItemSpec {
            item_id: item.into(),
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
    let acceptance = repo.accept(request, &command, &work, RUN, None).unwrap();
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
            task_control::Change::StartItem(item.into()),
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

/// The staged candidate record the envelope path parks before commit —
/// same shape `publish_bars` writes.
fn staged_raw(version_label: &str, bars: Vec<Bar>) -> Value {
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
    raw
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
    let raw = staged_raw(version_label, bars);
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
        raw: None,
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
            None,
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
    let refold = series::fold_bars(
        SYMBOL,
        Period::Day,
        series::SeriesShape::FORWARD_STOCK_CENTS,
        &all,
    )
    .unwrap();
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
            None,
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
    assert_eq!(bars[0].close_units, 200);
    assert_eq!(
        series::fold_bars(
            SYMBOL,
            Period::Day,
            series::SeriesShape::FORWARD_STOCK_CENTS,
            &bars
        )
        .unwrap(),
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
    running_task_multi_req(repo, count, "req-m")
}

/// `req` lets one test create several tasks — request ids dedupe.
fn running_task_multi_req(
    repo: &super::super::compact_repo::SqliteCompactMarket,
    count: usize,
    req: &str,
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
    let acceptance = repo.accept(req, &command, &work, RUN, None).unwrap();
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
    .unwrap_or_else(|e| panic!("start_item({item}) at rev {revision}: {e:?}"))
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
        open_units: 999,
        high_units: 999,
        low_units: 999,
        close_units: 999,
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

/// Runner semantics after a rejected commit: the item records its
/// failure so the next item may start (one running item per task).
fn fail_item(repo: &super::super::compact_repo::SqliteCompactMarket, task_id: &str, item: &str) {
    use crate::task_control::TaskRepository;
    let revision = repo.task(task_id).unwrap().task.state_revision;
    repo.transition(
        task_id,
        revision,
        RUN,
        task_control::Change::FinishItem(
            item.into(),
            task_control::ItemOutcome {
                success: false,
                error: Some("commit rejected".into()),
                evidence: None,
            },
        ),
    )
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
        .commit_range_no_change_typed(&cmd, &first.version_id, &range, None, None)
        .unwrap()
        .unwrap();
    let r2 = repo
        .commit_range_no_change_typed(&cmd, &first.version_id, &range, None, None)
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
        None,
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
            None,
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
    let new_bars = to_compact_bars(
        &[bar(0, 200), bar(1, 210)],
        Period::Day,
        crate::compact_market::PRICE_SCALE_CENTS,
    )
    .unwrap();
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
                adjustment: Adjustment::Forward,
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
                staged_version_id: None,
                defer_task_row: false,
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
            None,
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
        None,
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
    assert_eq!(after[0].close_units, 100);
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
            None,
        )
        .expect_err("source drift must be rejected");
    assert!(
        err.to_string().contains("drifts source or adjustment"),
        "unexpected rejection: {err}"
    );
    fail_item(&repo, &task_id, "item-3");

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
            None,
        )
        .expect_err("adjustment drift must be rejected");
    assert!(
        err.to_string().contains("drifts source or adjustment"),
        "unexpected rejection: {err}"
    );
    fail_item(&repo, &task_id, "item-4");

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
            None,
        )
        .expect_err("unit drift must be rejected");
    assert!(
        err.to_string().contains("drifts units"),
        "unexpected rejection: {err}"
    );
    fail_item(&repo, &task_id, "item-5");

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
            None,
        )
        .expect_err("non-complete coverage must be rejected");
    assert!(
        err.to_string().contains("coverage is not complete"),
        "unexpected rejection: {err}"
    );
    fail_item(&repo, &task_id, "item-6");

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
            raw: None,
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

    // Release the task's scope so a second collection task may be
    // accepted (one running scope per entity).
    let revision = repo.task(&task_id).unwrap().task.state_revision;
    repo.transition(
        &task_id,
        revision,
        RUN,
        task_control::Change::FinishExecution,
    )
    .unwrap();

    // Collection on an existing series -> Conflict (drift checks do not
    // apply to first-publish semantics). Batch owns the StartItem, so
    // the task is claimed but its item left pending.
    let (task2, _r2) = running_task_multi_req(&repo, 1, "req-m2");
    let mut again = candidate(&task2, ITEM, units());
    again.publication_id = "col-again".into();
    let single = vec![again];
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
                raw: None,
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
                raw: None,
            },
        },
    ];
    let out = repo.commit_update_batch(&task_id, &items).unwrap().unwrap();
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
    let (task_b, _r_b) = running_task_multi(&repo_b, 1);
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
        raw: None,
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
            None,
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
            None,
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

    // Same result_id under a different task -> Conflict (replay gate
    // fires on identity before any head/binding check).
    let err = repo
        .commit_range_ops(
            &command_at("task-other", "pub-2", rev2, "item-1"),
            &scope(),
            &append.version_id,
            &[bar(3, 130)],
            &[],
            &[bar(0, 100), bar(1, 110), bar(2, 120), bar(3, 130)],
            None,
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

    fn base_commit<'a>(task: &'a str, item: &'a str, run: &'a str, symbol: &'a str) -> Commit<'a> {
        Commit {
            result_id: "bind-1",
            task_id: task,
            item_id: item,
            run_id: run,
            occupancy_key: "",
            expected_task_revision: 1,
            entity: symbol,
            period: Period::Day,
            adjustment: Adjustment::Forward,
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
            staged_version_id: None,
            defer_task_row: false,
        }
    }

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

    // Correct binding commits — the event evidence must be a real
    // bundle, so the digests are written inside the transaction.
    repo.write(|c| {
        let evidence = evidence_store::put_evidence(c, EvidenceKind::Version, &version_evidence())?;
        let binding = put_generation_binding(
            c,
            "gen-1",
            &"0b".repeat(32),
            &"0c".repeat(32),
            &"0e".repeat(32),
            &generation_evidence(),
        )?;
        let mut w = base_commit(&task_id, ITEM, RUN, SYMBOL);
        w.occupancy_key = &occ;
        w.expected_task_revision = real_revision;
        w.change = SeriesChange::FirstPublish { bars: &[BAR0] };
        w.source_binding_digest = Some(binding);
        w.evidence_digest = Some(evidence);
        w.event_evidence_digest = evidence;
        commit_series_tx(c, &w)
    })
    .unwrap();
}

const BAR0: CompactBar = CompactBar {
    trade_day: 0,
    open_units: 100,
    high_units: 110,
    low_units: 90,
    close_units: 100,
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
            None,
        )
        .unwrap()
        .unwrap();
    let c = Connection::open(&path).unwrap();
    let cov: i64 = c
        .query_row(
            "SELECT coverage_status FROM market_series WHERE version_id=?1",
            [receipt.version_id.clone()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(cov, 1, "coverage must stay complete (code 1)");
    // canonical_json is decomposed ($evref parts) — read_evidence inflates.
    let row = series::read_series(&c, SYMBOL, Period::Day)
        .unwrap()
        .unwrap();
    let (kind, canonical) = evidence_store::read_evidence(&c, &row.evidence_digest).unwrap();
    assert_eq!(kind, EvidenceKind::Version);
    let ev: Value = serde_json::from_str(&canonical).unwrap();
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
            None,
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
        "INSERT INTO market_events(seq,entity,period,adjustment,kind,outcome,metadata_changed,new_content_digest,evidence_digest,task_id,item_id,task_revision,result_id,result_digest,created_at) \
         SELECT (SELECT max(seq)+1 FROM market_events),entity,period,adjustment,'append',outcome,metadata_changed,new_content_digest,evidence_digest,task_id,item_id,task_revision,'pub-x',result_digest,created_at \
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
            None,
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
            None,
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
                adjustment: Adjustment::Forward,
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
                staged_version_id: None,
                defer_task_row: false,
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

// ---------------------------------------------------------------------------
// S3 harness + yearly tests. `TaskHandle::item()` enforces the
// one-running-item rule: it finishes a still-running previous item before
// starting the next.
// ---------------------------------------------------------------------------

struct Repo {
    path: std::path::PathBuf,
    repo: super::super::compact_repo::SqliteCompactMarket,
    _dir: tempfile::TempDir,
}

impl Repo {
    fn new() -> Self {
        let dir = tempdir();
        let (path, repo) = repo(&dir);
        Repo {
            path,
            repo,
            _dir: dir,
        }
    }

    /// `count` pending items on the given scope/period; the task is claimed
    /// but no item is started — `item()` owns the StartItem lifecycle.
    fn task(&self, count: usize, period: &str, req: &str) -> TaskHandle {
        let scope = task_control::legacy::scope_id(SYMBOL, period);
        let items: Vec<ItemSpec> = (1..=count)
            .map(|index| ItemSpec {
                item_id: format!("item-{index}"),
                target: json!({"symbol": SYMBOL, "period": period}),
                progress_unit: "bars".into(),
                frozen_inputs: json!({}),
                scope_keys: std::collections::BTreeSet::from([scope.clone()]),
            })
            .collect();
        let command = task_control::Command {
            key: CommandKey {
                operation: "update_data".into(),
                dataset: if period == "year" {
                    "market.yearly".into()
                } else {
                    "market.daily".into()
                },
                contract_version: "v1".into(),
            },
            intent: json!({}),
        };
        let work = task_control::FrozenWork {
            items,
            scopes: std::collections::BTreeSet::from([scope]),
            shared_evidence: None,
        };
        let acceptance = self.repo.accept(req, &command, &work, RUN, None).unwrap();
        let task_id = match acceptance.record.outcome {
            task_control::RequestOutcome::Task(snapshot) => snapshot.task.task_id.clone(),
            _ => panic!("expected task outcome"),
        };
        let revision = self.repo.task(&task_id).unwrap().task.state_revision;
        self.repo
            .transition(&task_id, revision, RUN, task_control::Change::Claim)
            .unwrap();
        TaskHandle {
            task_id,
            prefix: format!("{period}-{req}"),
            next_item: 0,
            running: None,
        }
    }
}

struct TaskHandle {
    task_id: String,
    prefix: String,
    next_item: usize,
    running: Option<String>,
}

impl TaskHandle {
    /// Start the next item, finishing the previous one if a rejected commit
    /// left it running. Returns (item_id, publication_id, revision).
    /// Publication ids carry the task's req prefix — `result_id` is
    /// globally unique.
    fn item(
        &mut self,
        repo: &super::super::compact_repo::SqliteCompactMarket,
    ) -> (String, String, u64) {
        if let Some(prev) = self.running.take() {
            let still_running = repo
                .task(&self.task_id)
                .unwrap()
                .items
                .iter()
                .any(|i| i.spec.item_id == prev && i.stage == task_control::Stage::Running);
            if still_running {
                fail_item(repo, &self.task_id, &prev);
            }
        }
        self.next_item += 1;
        let item = format!("item-{}", self.next_item);
        let pub_id = format!("{}-pub-{}", self.prefix, self.next_item);
        let rev = start_item_id(repo, &self.task_id, &item);
        self.running = Some(item.clone());
        (item, pub_id, rev)
    }
}

/// Epoch day for a calendar date — yearly bar `trade_day` is the last
/// trading day of its year.
fn day(y: i32, m: u32, d: u32) -> i64 {
    compact_market::epoch_day(NaiveDate::from_ymd_opt(y, m, d).unwrap())
}

fn ybar(y: i32, close: i64) -> CompactBar {
    CompactBar {
        trade_day: day(y, 12, 31),
        open_units: close,
        high_units: close + 10,
        low_units: close - 10,
        close_units: close,
        volume: Some(1),
        amount: None,
        r7_raw: None,
    }
}

#[test]
fn yearly_first_publish_creates_series_lineage_and_event() {
    // Tracer bullet (S3): the real commit gate writes the yearly series,
    // affected bars, lineage and the first_publish event in one tx.
    let r = Repo::new();
    let mut daily_task = r.task(1, "day", "req-d");
    let (_i1, pub1, rev1) = daily_task.item(&r.repo);
    let first = publish_bars(
        &r.repo,
        &daily_task.task_id,
        rev1,
        &pub1,
        "v1",
        vec![bar(0, 100), bar(1, 110)],
    );

    let mut yearly_task = r.task(1, "year", "req-y");
    let (y_item, y_pub, y_rev) = yearly_task.item(&r.repo);
    let occupancy = task_control::legacy::scope_id(SYMBOL, "year");
    let as_of = day(2026, 10, 12);
    let c0 = Connection::open(&r.path).unwrap();
    let daily = series::read_series(&c0, SYMBOL, Period::Day)
        .unwrap()
        .unwrap();
    r.repo
        .write(|c| {
            let evidence =
                evidence_store::put_evidence(c, EvidenceKind::Version, &version_evidence())?;
            let y_evidence = evidence_store::put_evidence(
                c,
                EvidenceKind::Yearly,
                &json!({"base_daily_version_id": daily.version_id}),
            )?;
            commit_series_tx(
                c,
                &Commit {
                    result_id: &y_pub,
                    task_id: &yearly_task.task_id,
                    item_id: &y_item,
                    run_id: RUN,
                    occupancy_key: &occupancy,
                    expected_task_revision: y_rev,
                    entity: SYMBOL,
                    period: Period::Year,
                    adjustment: Adjustment::Forward,
                    base_version_id: None,
                    change: SeriesChange::Yearly {
                        upserts: &[ybar(2020, 100)],
                        daily_base_digest: daily.content_digest,
                        algorithm_version: "local-natural-year-v1",
                        as_of_day: as_of,
                    },
                    range_start_day: 0,
                    range_end_day: 0,
                    algorithm_version: "local-natural-year-v1",
                    window_digest: None,
                    source_binding_digest: None,
                    evidence_digest: Some(evidence),
                    coverage_status: Some(crate::market_data::CoverageStatus::Complete),
                    metadata_changed: false,
                    event_evidence_digest: y_evidence,
                    published_at: None,
                    staged_version_id: None,
                    defer_task_row: false,
                },
            )
        })
        .unwrap();

    let c = Connection::open(&r.path).unwrap();
    let yearly = series::read_series(&c, SYMBOL, Period::Year)
        .unwrap()
        .expect("yearly series must exist");
    let bars = series::all_bars(&c, yearly.series_key).unwrap();
    assert_eq!(bars.len(), 1);
    assert_eq!(bars[0].trade_day, day(2020, 12, 31));
    // first_publish event on the yearly series.
    let kind: String = c
        .query_row(
            "SELECT kind FROM market_events WHERE entity=?1 AND period=2 ORDER BY seq DESC LIMIT 1",
            [SYMBOL],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(kind, "first_publish");
    // lineage names the daily base the derivation read.
    let lineage = series::yearly_lineage(&c, yearly.series_key)
        .unwrap()
        .expect("lineage must exist");
    assert_eq!(lineage.base_daily_digest, daily.content_digest);
    assert_eq!(lineage.base_event_seq, daily.last_event_seq);
    assert_eq!(lineage.as_of_day, as_of);
    assert_eq!(first.version_id, daily.version_id);
}

// ---------------------------------------------------------------------------
// S3 batch 2: yearly commit through the real YearlyResultPlan surface —
// rederive subset, no_change lineage advance, CAS race, result_id replay,
// and the stale-classification read-out (§8.1-7/8, §8.2, C3).
// ---------------------------------------------------------------------------

fn mk_year_version(version_id: &str, bars: Vec<Bar>, base_daily_id: &str) -> EffectiveVersion {
    let (first, last) = (bars[0].date, bars[bars.len() - 1].date);
    let mut version = EffectiveVersion {
        version_id: version_id.into(),
        symbol: SYMBOL.into(),
        period: Period::Year,
        adjustment: Adjustment::Forward,
        source: "official".into(),
        records: bars,
        published_at: DateTime::from_timestamp(1_760_000_000, 0).unwrap(),
        validation: ValidationSummary {
            record_count: 0,
            unique_record_count: 0,
            duplicate_count: 0,
        },
        unit_currency: units(),
        adjustment_evidence: AdjustmentEvidence::confirmed("qfq", "bound"),
        yearly_derivation: Some(crate::market_data::YearlyDerivation {
            base_daily_version_id: base_daily_id.into(),
            algorithm_version: "local-natural-year-v1".into(),
            first_trading_date: first,
            last_trading_date: last,
        }),
    };
    let (records, validation) = crate::publish::validate_and_deduplicate(&mk_batch(&version))
        .expect("yearly candidate validates");
    version.records = records;
    version.validation = validation;
    version
}

fn mk_batch(version: &EffectiveVersion) -> MarketDataBatch {
    MarketDataBatch {
        symbol: version.symbol.clone(),
        period: version.period,
        adjustment: Adjustment::Forward,
        bars: version.records.clone(),
        provenance: Provenance {
            source: version.source.clone(),
            source_period: "day".into(),
            source_adjustment: "qfq".into(),
            symbol: version.symbol.clone(),
            acquired_at: version.published_at,
            adapter_version: "local-yearly-publication-v1".into(),
            yearly_derivation: version.yearly_derivation.clone(),
            snapshot_inputs: None,
        },
        coverage: CoverageEvidence::complete("selected years verified"),
        unit_currency: units(),
        adjustment_evidence: AdjustmentEvidence::confirmed("qfq", "bound"),
    }
}

fn checked(version: &EffectiveVersion) -> compat::CheckedRecord {
    let mut raw = serde_json::to_value(version).unwrap();
    raw["batch"] = serde_json::to_value(mk_batch(version)).unwrap();
    raw["integrity_sha256"] = json!(compat::legacy_digest(&raw).unwrap());
    compat::check(&raw).unwrap()
}

fn daily_checked(r: &Repo) -> compat::CheckedRecord {
    let c = Connection::open(&r.path).unwrap();
    let daily = series::read_series(&c, SYMBOL, Period::Day)
        .unwrap()
        .unwrap();
    let bars = series::all_bars(&c, daily.series_key).unwrap();
    let bars = bars
        .iter()
        .map(|b| b.to_bar(Period::Day, daily.price_scale))
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let mut version = EffectiveVersion {
        version_id: daily.version_id.clone(),
        symbol: SYMBOL.into(),
        period: Period::Day,
        adjustment: Adjustment::Forward,
        source: "official".into(),
        records: bars,
        published_at: DateTime::from_timestamp(1_760_000_000, 0).unwrap(),
        validation: ValidationSummary {
            record_count: 0,
            unique_record_count: 0,
            duplicate_count: 0,
        },
        unit_currency: units(),
        adjustment_evidence: AdjustmentEvidence::confirmed("qfq", "bound"),
        yearly_derivation: None,
    };
    let (records, validation) =
        crate::publish::validate_and_deduplicate(&mk_batch(&version)).unwrap();
    version.records = records;
    version.validation = validation;
    checked(&version)
}

/// Commit the yearly candidate through the REAL `commit_yearly` plan path.
/// `replay` resubmits the exact recorded identity (publication id + item
/// id) without starting a fresh item — the receipt must come straight back.
fn commit_yearly_plan(
    r: &Repo,
    task: &mut TaskHandle,
    candidate: EffectiveVersion,
    base: &compat::CheckedRecord,
    prior: Option<compat::CheckedRecord>,
    result_id: Option<&str>,
    replay_item: Option<&str>,
) -> Result<crate::market_storage::Receipt, crate::market_storage::Error> {
    let (item, pub_id, rev) = match replay_item {
        Some(item) => (item.to_string(), String::new(), 0),
        None => task.item(&r.repo),
    };
    let publication_id = result_id.map(str::to_string).unwrap_or(pub_id);
    let command = Publication {
        publication_id,
        version_id: candidate.version_id.clone(),
        task_id: task.task_id.clone(),
        item_id: item,
        run_id: RUN.into(),
        expected_revision: rev,
        intent: PublishIntent::CurrentHead {
            expected_daily_head: base.version().version_id.clone(),
            expected_yearly_head: prior.as_ref().map(|p| p.version().version_id.clone()),
        },
    };
    let record = checked(&candidate);
    let no_change = prior
        .as_ref()
        .is_some_and(|p| p.raw()["integrity_sha256"] == record.raw()["integrity_sha256"]);
    let plan = crate::yearly_publication::results::YearlyResultPlan {
        publication: PublishPlan {
            command,
            record,
            required_heads: vec![],
            promote: None,
            base_digest: None,
            raw: None,
        },
        base: base.clone(),
        prior,
        no_change,
        audit: json!({}),
        audit_payloads: vec![],
    };
    r.repo.commit_yearly(&plan)
}

#[test]
fn yearly_rederive_rewrites_only_affected_years() {
    // §8.1-8: an affected-year upsert replaces that year's bar only; every
    // other stored year row survives untouched.
    let r = Repo::new();
    let mut daily_task = r.task(1, "day", "req-d8");
    let (_i, pub_d, rev_d) = daily_task.item(&r.repo);
    let daily = publish_bars(
        &r.repo,
        &daily_task.task_id,
        rev_d,
        &pub_d,
        "v1",
        vec![bar(0, 100), bar(1, 110)],
    );
    let base = daily_checked(&r);
    let mut yearly_task = r.task(3, "year", "req-y8");

    // First yearly publish: 2020 + 2021.
    let first_yearly = commit_yearly_plan(
        &r,
        &mut yearly_task,
        mk_year_version(
            "y-v1",
            vec![year_bar(2020, 100), year_bar(2021, 200)],
            &daily.version_id,
        ),
        &base,
        None,
        None,
        None,
    )
    .unwrap();

    // Rederive with only 2021 affected: 2021 changes, 2020 stays.
    let prior = {
        let c = Connection::open(&r.path).unwrap();
        let row = series::read_series(&c, SYMBOL, Period::Year)
            .unwrap()
            .unwrap();
        mk_year_version(
            &row.version_id,
            vec![year_bar(2020, 100), year_bar(2021, 200)],
            &daily.version_id,
        )
    };
    let second = commit_yearly_plan(
        &r,
        &mut yearly_task,
        mk_year_version(
            "y-v2",
            vec![year_bar(2020, 100), year_bar(2021, 220)],
            &daily.version_id,
        ),
        &base,
        Some(checked(&prior)),
        None,
        None,
    )
    .unwrap();
    assert_ne!(second.version_id, first_yearly.version_id);

    let c = Connection::open(&r.path).unwrap();
    let yearly = series::read_series(&c, SYMBOL, Period::Year)
        .unwrap()
        .unwrap();
    let bars = series::all_bars(&c, yearly.series_key).unwrap();
    assert_eq!(bars.len(), 2);
    assert_eq!(bars[0].close_units, 100, "2020 bar must be untouched");
    assert_eq!(bars[1].close_units, 220, "2021 bar rederived");
    // yearly_rederive event with daily base linkage.
    let (kind, base_digest): (String, Vec<u8>) = c
        .query_row(
            "SELECT kind,base_content_digest FROM market_events WHERE entity=?1 AND period=2 ORDER BY seq DESC LIMIT 1",
            [SYMBOL],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(kind, "yearly_rederive");
    let daily_head = series::read_series(&c, SYMBOL, Period::Day)
        .unwrap()
        .unwrap();
    assert_eq!(
        series::version_id(&digest32(&base_digest)),
        daily_head.version_id
    );
}

#[test]
fn yearly_no_change_advances_lineage_base() {
    // A second derivation with identical content logs no_change but still
    // advances the lineage to the newer daily base (Fresh again).
    let r = Repo::new();
    let mut daily_task = r.task(2, "day", "req-d9");
    let (_i, pub_d, rev_d) = daily_task.item(&r.repo);
    let daily = publish_bars(
        &r.repo,
        &daily_task.task_id,
        rev_d,
        &pub_d,
        "v1",
        vec![bar(0, 100), bar(1, 110)],
    );
    let base = daily_checked(&r);
    let mut yearly_task = r.task(2, "year", "req-y9");
    commit_yearly_plan(
        &r,
        &mut yearly_task,
        mk_year_version("y-v1", vec![year_bar(2020, 100)], &daily.version_id),
        &base,
        None,
        None,
        None,
    )
    .unwrap();

    // Daily append: the yearly now trails the current daily (Behind).
    let (_i2, pub_d2, rev_d2) = daily_task.item(&r.repo);
    r.repo
        .commit_range_ops(
            &command_at(&daily_task.task_id, &pub_d2, rev_d2, "item-2"),
            &scope(),
            &daily.version_id,
            &[bar(2, 120)],
            &[],
            &[bar(0, 100), bar(1, 110), bar(2, 120)],
            None,
        )
        .unwrap()
        .unwrap();
    let base2 = daily_checked(&r);
    let c = Connection::open(&r.path).unwrap();
    let yearly = series::read_series(&c, SYMBOL, Period::Year)
        .unwrap()
        .unwrap();
    assert!(matches!(
        series::yearly_state(&c, &yearly).unwrap(),
        series::YearlyState::Behind { .. }
    ));

    // Same yearly content against the new daily base -> no_change, but the
    // lineage moves forward so the row is Fresh again.
    let prior = mk_year_version(
        &yearly.version_id,
        vec![year_bar(2020, 100)],
        &base2.version().version_id,
    );
    commit_yearly_plan(
        &r,
        &mut yearly_task,
        mk_year_version(
            "y-v2",
            vec![year_bar(2020, 100)],
            &base2.version().version_id,
        ),
        &base2,
        Some(checked(&prior)),
        None,
        None,
    )
    .unwrap();
    let yearly = series::read_series(&c, SYMBOL, Period::Year)
        .unwrap()
        .unwrap();
    let kind: String = c
        .query_row(
            "SELECT kind FROM market_events WHERE entity=?1 AND period=2 ORDER BY seq DESC LIMIT 1",
            [SYMBOL],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(kind, "no_change");
    assert_eq!(
        series::yearly_state(&c, &yearly).unwrap(),
        series::YearlyState::Fresh,
        "no_change must advance the lineage base to the new daily"
    );
}

#[test]
fn yearly_commit_stale_daily_base_fails_closed() {
    // SPEC §5: the derivation's daily base must still be the current daily
    // head inside the write tx — a raced daily append lands `Stale`, the
    // item is not consumed, and a re-derived commit succeeds.
    let r = Repo::new();
    let mut daily_task = r.task(2, "day", "req-d10");
    let (_i, pub_d, rev_d) = daily_task.item(&r.repo);
    let daily = publish_bars(
        &r.repo,
        &daily_task.task_id,
        rev_d,
        &pub_d,
        "v1",
        vec![bar(0, 100), bar(1, 110)],
    );
    let stale_base = daily_checked(&r);

    // Race: the daily head moves before the yearly commit lands.
    let (_i2, pub_d2, rev_d2) = daily_task.item(&r.repo);
    let moved = r
        .repo
        .commit_range_ops(
            &command_at(&daily_task.task_id, &pub_d2, rev_d2, "item-2"),
            &scope(),
            &daily.version_id,
            &[bar(2, 120)],
            &[],
            &[bar(0, 100), bar(1, 110), bar(2, 120)],
            None,
        )
        .unwrap()
        .unwrap();

    let mut yearly_task = r.task(2, "year", "req-y10");
    let err = commit_yearly_plan(
        &r,
        &mut yearly_task,
        mk_year_version("y-v1", vec![year_bar(2020, 100)], &daily.version_id),
        &stale_base,
        None,
        None,
        None,
    )
    .expect_err("stale daily base must be rejected");
    assert!(
        matches!(err, crate::market_storage::Error::Stale(_)),
        "expected Stale, got {err:?}"
    );
    // The item stays consumable: a re-derived commit lands first_publish.
    let fresh_base = daily_checked(&r);
    let receipt = commit_yearly_plan(
        &r,
        &mut yearly_task,
        mk_year_version("y-v2", vec![year_bar(2020, 100)], &moved.version_id),
        &fresh_base,
        None,
        None,
        None,
    )
    .unwrap();
    let c = Connection::open(&r.path).unwrap();
    let yearly = series::read_series(&c, SYMBOL, Period::Year)
        .unwrap()
        .unwrap();
    assert_eq!(yearly.version_id, receipt.version_id);
    assert_eq!(
        series::yearly_state(&c, &yearly).unwrap(),
        series::YearlyState::Fresh
    );
}

#[test]
fn yearly_rederive_replays_result_id() {
    // §8.2-16 yearly variant: re-submitting the same result_id replays the
    // recorded receipt — one event, one result.
    let r = Repo::new();
    let mut daily_task = r.task(1, "day", "req-d11");
    let (_i, pub_d, rev_d) = daily_task.item(&r.repo);
    let daily = publish_bars(
        &r.repo,
        &daily_task.task_id,
        rev_d,
        &pub_d,
        "v1",
        vec![bar(0, 100), bar(1, 110)],
    );
    let base = daily_checked(&r);
    let mut yearly_task = r.task(2, "year", "req-y11");
    commit_yearly_plan(
        &r,
        &mut yearly_task,
        mk_year_version("y-v1", vec![year_bar(2020, 100)], &daily.version_id),
        &base,
        None,
        Some("y-result-1"),
        None,
    )
    .unwrap();

    let prior = {
        let c = Connection::open(&r.path).unwrap();
        let row = series::read_series(&c, SYMBOL, Period::Year)
            .unwrap()
            .unwrap();
        mk_year_version(
            &row.version_id,
            vec![year_bar(2020, 100)],
            &daily.version_id,
        )
    };
    let replayed = commit_yearly_plan(
        &r,
        &mut yearly_task,
        mk_year_version("y-v2", vec![year_bar(2020, 120)], &daily.version_id),
        &base,
        Some(checked(&prior)),
        Some("y-result-1"),
        Some("item-1"),
    )
    .unwrap();
    assert_eq!(replayed.publication_id, "y-result-1");
    let c = Connection::open(&r.path).unwrap();
    let events: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM market_events WHERE entity=?1 AND period=2",
            [SYMBOL],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(events, 1, "replay must not insert a second yearly event");
    // The replayed commit did not land the drifted content either.
    let yearly = series::read_series(&c, SYMBOL, Period::Year)
        .unwrap()
        .unwrap();
    let bars = series::all_bars(&c, yearly.series_key).unwrap();
    assert_eq!(bars[0].close_units, 100);
}

#[test]
fn daily_semantics_change_invalidates_yearly_but_metadata_does_not() {
    // C3: a daily commit whose version evidence differs in {source,
    // unit_currency, adjustment_evidence} flips the yearly row to
    // Invalid(value_semantics_changed); a metadata-only event carrying the
    // SAME semantics leaves it Behind.
    let r = Repo::new();
    let mut daily_task = r.task(4, "day", "req-d12");
    let (_i, pub_d, rev_d) = daily_task.item(&r.repo);
    let daily = publish_bars(
        &r.repo,
        &daily_task.task_id,
        rev_d,
        &pub_d,
        "v1",
        vec![bar(0, 100), bar(1, 110)],
    );
    let base = daily_checked(&r);
    let mut yearly_task = r.task(2, "year", "req-y12");
    commit_yearly_plan(
        &r,
        &mut yearly_task,
        mk_year_version("y-v1", vec![year_bar(2020, 100)], &daily.version_id),
        &base,
        None,
        None,
        None,
    )
    .unwrap();

    // A metadata event with unchanged semantics -> Behind, not Invalid.
    let (_i2, pub_d2, rev_d2) = daily_task.item(&r.repo);
    let metadata = metadata_gen("gen-b", &"be".repeat(32));
    r.repo
        .commit_range_ops_typed(
            &command_at(&daily_task.task_id, &pub_d2, rev_d2, "item-2"),
            &scope(),
            &daily.version_id,
            &[bar(2, 120)],
            &[],
            Some(&metadata),
            None,
            None,
        )
        .unwrap()
        .unwrap();
    let c = Connection::open(&r.path).unwrap();
    let yearly = series::read_series(&c, SYMBOL, Period::Year)
        .unwrap()
        .unwrap();
    assert!(
        matches!(
            series::yearly_state(&c, &yearly).unwrap(),
            series::YearlyState::Behind { .. }
        ),
        "metadata-only semantics must not invalidate the yearly row"
    );
    let sem: i64 = c
        .query_row(
            "SELECT semantics_changed FROM market_events WHERE entity=?1 AND period=1 ORDER BY seq DESC LIMIT 1",
            [SYMBOL],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(sem, 0, "same-semantics metadata lands semantics_changed=0");

    // A REPLACE carrying different unit evidence flips semantics_changed=1
    // and the yearly row becomes Invalid(value_semantics_changed).
    let daily_head = series::read_series(&c, SYMBOL, Period::Day)
        .unwrap()
        .unwrap();
    let (_i3, pub_d3, rev_d3) = daily_task.item(&r.repo);
    let mut drifted_units = units();
    drifted_units.price_unit = EvidenceValue::confirmed("fen", "drift");
    let replace_bars: Vec<CompactBar> = [bar(0, 100), bar(1, 110), bar(2, 120)]
        .iter()
        .map(|b| {
            CompactBar::from_bar(b, Period::Day, crate::compact_market::PRICE_SCALE_CENTS).unwrap()
        })
        .collect();
    r.repo
        .write(|c| {
            let evidence = evidence_store::put_evidence(
                c,
                EvidenceKind::Version,
                &json!(VersionEvidence {
                    source: "official".into(),
                    coverage_reason: "full history verified".into(),
                    unit_currency: drifted_units.clone(),
                    adjustment_evidence: AdjustmentEvidence::confirmed("qfq", "bound"),
                }),
            )?;
            let continuity = evidence_store::put_evidence(
                c,
                EvidenceKind::Continuity,
                &json!({"mode": "full", "proven": true}),
            )?;
            commit_series_tx(
                c,
                &Commit {
                    result_id: &pub_d3,
                    task_id: &daily_task.task_id,
                    item_id: "item-3",
                    run_id: RUN,
                    occupancy_key: &scope().occupancy_key(),
                    expected_task_revision: rev_d3,
                    entity: SYMBOL,
                    period: Period::Day,
                    adjustment: Adjustment::Forward,
                    base_version_id: Some(daily_head.version_id.as_str()),
                    change: SeriesChange::Replace {
                        bars: &replace_bars,
                    },
                    range_start_day: 0,
                    range_end_day: 2,
                    algorithm_version: "test",
                    window_digest: None,
                    source_binding_digest: Some(daily_head.source_binding_digest),
                    evidence_digest: Some(evidence),
                    coverage_status: Some(crate::market_data::CoverageStatus::Complete),
                    metadata_changed: true,
                    event_evidence_digest: continuity,
                    published_at: None,
                    staged_version_id: None,
                    defer_task_row: false,
                },
            )
        })
        .unwrap();
    let sem: i64 = c
        .query_row(
            "SELECT semantics_changed FROM market_events WHERE entity=?1 AND period=1 ORDER BY seq DESC LIMIT 1",
            [SYMBOL],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(sem, 1, "drifted unit evidence must flag semantics_changed");
    let yearly = series::read_series(&c, SYMBOL, Period::Year)
        .unwrap()
        .unwrap();
    // Ruling: a replace tail wins over a pure evidence move — both fail
    // closed; the row reports the content-level reason.
    assert_eq!(
        series::yearly_state(&c, &yearly).unwrap(),
        series::YearlyState::Invalid(series::YearlyInvalidReason::DailyReplaced)
    );

    // A pure APPEND carrying drifted evidence (no replace in the tail)
    // surfaces the SemanticsChanged reason. Re-derive the yearly first so
    // the lineage base moves past the replace.
    let daily_head = series::read_series(&c, SYMBOL, Period::Day)
        .unwrap()
        .unwrap();
    let base2 = daily_checked(&r);
    let yearly = series::read_series(&c, SYMBOL, Period::Year)
        .unwrap()
        .unwrap();
    let prior = mk_year_version(
        &yearly.version_id,
        vec![year_bar(2020, 100)],
        &daily_head.version_id,
    );
    commit_yearly_plan(
        &r,
        &mut yearly_task,
        mk_year_version("y-v2", vec![year_bar(2020, 100)], &daily_head.version_id),
        &base2,
        Some(checked(&prior)),
        None,
        None,
    )
    .unwrap();
    let daily_head = series::read_series(&c, SYMBOL, Period::Day)
        .unwrap()
        .unwrap();
    let (_i4, pub_d4, rev_d4) = daily_task.item(&r.repo);
    // The head already carries the fen drift — the append must move the
    // semantics AGAIN for the flag to read 1.
    let mut drifted2 = drifted_units.clone();
    drifted2.price_unit = EvidenceValue::confirmed("li", "drift-again");
    let append_bars: Vec<CompactBar> = [CompactBar::from_bar(
        &bar(3, 130),
        Period::Day,
        crate::compact_market::PRICE_SCALE_CENTS,
    )
    .unwrap()]
    .into_iter()
    .collect();
    r.repo
        .write(|c| {
            let evidence = evidence_store::put_evidence(
                c,
                EvidenceKind::Version,
                &json!(VersionEvidence {
                    source: "official".into(),
                    coverage_reason: "full history verified".into(),
                    unit_currency: drifted2.clone(),
                    adjustment_evidence: AdjustmentEvidence::confirmed("qfq", "bound"),
                }),
            )?;
            let continuity = evidence_store::put_evidence(
                c,
                EvidenceKind::Continuity,
                &json!({"mode": "prefix", "proven": true}),
            )?;
            commit_series_tx(
                c,
                &Commit {
                    result_id: &pub_d4,
                    task_id: &daily_task.task_id,
                    item_id: "item-4",
                    run_id: RUN,
                    occupancy_key: &scope().occupancy_key(),
                    expected_task_revision: rev_d4,
                    entity: SYMBOL,
                    period: Period::Day,
                    adjustment: Adjustment::Forward,
                    base_version_id: Some(daily_head.version_id.as_str()),
                    change: SeriesChange::Append {
                        upserts: &append_bars,
                    },
                    range_start_day: 3,
                    range_end_day: 3,
                    algorithm_version: "test",
                    window_digest: None,
                    source_binding_digest: Some(daily_head.source_binding_digest),
                    evidence_digest: Some(evidence),
                    coverage_status: Some(crate::market_data::CoverageStatus::Complete),
                    metadata_changed: true,
                    event_evidence_digest: continuity,
                    published_at: None,
                    staged_version_id: None,
                    defer_task_row: false,
                },
            )
        })
        .unwrap();
    let sem: i64 = c
        .query_row(
            "SELECT semantics_changed FROM market_events WHERE entity=?1 AND period=1 ORDER BY seq DESC LIMIT 1",
            [SYMBOL],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(sem, 1, "pure append with drifted evidence flags semantics");
    let yearly = series::read_series(&c, SYMBOL, Period::Year)
        .unwrap()
        .unwrap();
    assert_eq!(
        series::yearly_state(&c, &yearly).unwrap(),
        series::YearlyState::Invalid(series::YearlyInvalidReason::SemanticsChanged)
    );
}

fn year_bar(y: i32, close: i64) -> Bar {
    CompactBar {
        trade_day: day(y, 12, 31),
        open_units: close,
        high_units: close + 10,
        low_units: close - 10,
        close_units: close,
        volume: Some(100),
        amount: Some("1000".into()),
        r7_raw: None,
    }
    .to_bar(Period::Year, crate::compact_market::PRICE_SCALE_CENTS)
    .unwrap()
}

fn digest32(bytes: &[u8]) -> Digest {
    bytes.try_into().expect("32-byte digest")
}

#[test]
fn catalog_roundtrip_on_compact_schema_uses_bound_tasks() {
    // N1: the worker builds the catalog executor from `market.tasks()`;
    // on schema 21 that handle is coordinator-bound — catalog publish /
    // read-back / task binding must work through it.
    use crate::catalog::{
        CatalogSnapshot, CompletenessEvidence, Exchange, IndustryInfo, SecurityRecord, SecurityType,
    };
    use crate::sqlite_catalog::SqliteCatalog;

    let dir = tempdir();
    let (_path, repo) = repo(&dir);
    let catalog = SqliteCatalog::new(repo.tasks());
    let now = Utc::now();
    let snapshot = CatalogSnapshot {
        source: "fixture:catalog".into(),
        acquired_at: now,
        completeness: CompletenessEvidence::Complete { expected_count: 1 },
        records: vec![SecurityRecord {
            symbol: "600006".into(),
            name: "样例".into(),
            exchange: Exchange::Shanghai,
            security_type: SecurityType::AShare,
            source_security_id: "sh600006".into(),
            industry: IndustryInfo::Missing {
                reason: "fixture unknown".into(),
            },
            source: "fixture:catalog".into(),
            acquired_at: now,
        }],
    };
    catalog
        .publish_fixture("cat-v1", snapshot.clone(), None)
        .unwrap();
    let facts = catalog.selection_facts().unwrap();
    assert_eq!(facts.catalog_head.as_deref(), Some("cat-v1"));
    assert_eq!(catalog.fixed("cat-v1").unwrap().records.len(), 1);
    assert!(matches!(
        catalog.publish_fixture("cat-v2", snapshot, Some("stale")),
        Err(crate::market_storage::Error::Conflict(_))
    ));
    assert_eq!(
        catalog.selection_facts().unwrap().catalog_head.as_deref(),
        Some("cat-v1")
    );
    // The bound task handle still works on the v21 file (accept + read).
    let accepted = repo.tasks().accept(
        "req-catalog-bound",
        &crate::task_control::Command {
            key: crate::task_control::CommandKey {
                operation: "collect".into(),
                dataset: "market.daily".into(),
                contract_version: "v1".into(),
            },
            intent: serde_json::json!({}),
        },
        &crate::task_control::FrozenWork {
            items: vec![crate::task_control::ItemSpec {
                item_id: "i1".into(),
                target: serde_json::json!({"symbol": SYMBOL}),
                progress_unit: "security_period".into(),
                frozen_inputs: serde_json::json!({}),
                scope_keys: std::collections::BTreeSet::from(["scope-1".to_string()]),
            }],
            scopes: std::collections::BTreeSet::from(["scope-1".to_string()]),
            shared_evidence: None,
        },
        RUN,
        None,
    );
    assert!(accepted.is_ok());
}

#[test]
fn rejected_item_leaves_head_and_task_state_untouched() {
    // N2 storage side: a commit that fails the binding gate leaves the
    // series head and records the item's failure — the item cannot leak
    // a partial write, and the task revision reflects only the rejection.
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

    // Foreign item rejected by the binding gate.
    let mut other_task_items: Vec<ItemSpec> = Vec::new();
    let other_scope = task_control::legacy::scope_id(SYMBOL, "year");
    other_task_items.push(ItemSpec {
        item_id: "item-1".into(),
        target: json!({"symbol": SYMBOL, "period": "year"}),
        progress_unit: "bars".into(),
        frozen_inputs: json!({}),
        scope_keys: std::collections::BTreeSet::from([other_scope.clone()]),
    });
    let other_command = task_control::Command {
        key: CommandKey {
            operation: "update_data".into(),
            dataset: "market.yearly".into(),
            contract_version: "v1".into(),
        },
        intent: json!({}),
    };
    let other_work = task_control::FrozenWork {
        items: other_task_items,
        scopes: std::collections::BTreeSet::from([other_scope]),
        shared_evidence: None,
    };
    let other_task = match repo
        .accept("req-other", &other_command, &other_work, RUN, None)
        .unwrap()
        .record
        .outcome
    {
        task_control::RequestOutcome::Task(snapshot) => snapshot.task.task_id.clone(),
        _ => panic!("expected task outcome"),
    };
    let other_rev = repo.task(&other_task).unwrap().task.state_revision;
    repo.transition(&other_task, other_rev, RUN, task_control::Change::Claim)
        .unwrap();
    let rev_other = start_item_id(&repo, &other_task, "item-1");
    let _ = rev_other;
    let c = Connection::open(dir.path().join("market.db")).unwrap();
    let head_before = series::read_series(&c, SYMBOL, Period::Day)
        .unwrap()
        .unwrap();
    let upserts = to_compact_bars(
        &[bar(2, 120)],
        Period::Day,
        crate::compact_market::PRICE_SCALE_CENTS,
    )
    .unwrap();
    let err = repo
        .write(|c| {
            let evidence =
                evidence_store::put_evidence(c, EvidenceKind::Version, &version_evidence())?;
            let continuity = evidence_store::put_evidence(
                c,
                EvidenceKind::Continuity,
                &json!({"mode": "prefix", "proven": true}),
            )?;
            commit_series_tx(
                c,
                &Commit {
                    result_id: "pub-cross",
                    task_id: &task_id,
                    item_id: "item-1",
                    run_id: RUN,
                    occupancy_key: &scope().occupancy_key(),
                    expected_task_revision: rev_other,
                    entity: SYMBOL,
                    period: Period::Day,
                    adjustment: Adjustment::Forward,
                    base_version_id: Some(&first.version_id),
                    change: SeriesChange::Append { upserts: &upserts },
                    range_start_day: 2,
                    range_end_day: 2,
                    algorithm_version: "test",
                    window_digest: None,
                    source_binding_digest: None,
                    evidence_digest: Some(evidence),
                    coverage_status: None,
                    metadata_changed: false,
                    event_evidence_digest: continuity,
                    published_at: None,
                    staged_version_id: None,
                    defer_task_row: false,
                },
            )
        })
        .expect_err("cross-task item must be rejected");
    assert!(matches!(err, Error::Conflict(_) | Error::Integrity(_)));

    let c = Connection::open(dir.path().join("market.db")).unwrap();
    let head_after = series::read_series(&c, SYMBOL, Period::Day)
        .unwrap()
        .unwrap();
    assert_eq!(
        head_after.version_id, head_before.version_id,
        "rejected item must not advance the head"
    );
    assert_eq!(head_after.last_event_seq, head_before.last_event_seq);
    let events: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM market_events WHERE result_id='pub-cross'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(events, 0, "rejected commit leaves no event row");

    // The bound item itself stays usable: a correct commit still lands.
    let rev2 = start_item_id(&repo, &task_id, "item-2");
    repo.commit_range_ops(
        &command_at(&task_id, "pub-2", rev2, "item-2"),
        &scope(),
        &first.version_id,
        &[bar(2, 120)],
        &[],
        &[bar(0, 100), bar(1, 110), bar(2, 120)],
        None,
    )
    .unwrap()
    .unwrap();
}

// ---------------------------------------------------------------------------
// staged_records lifecycle: every commit path consumes its candidate inside
// the commit transaction; rows orphaned by failed/cancelled item runs are
// bounded by the stage-time TTL sweep (fix/staged-records-gc).
// ---------------------------------------------------------------------------

#[test]
fn typed_commit_consumes_a_staged_orphan_of_the_same_item() {
    // An envelope attempt that staged `range-version-{item}` and then
    // failed leaves the row behind; the item's later typed commit carries
    // the same candidate id and reclaims it inside the commit transaction.
    let dir = tempdir();
    let (path, repo) = repo(&dir);
    let (task_id, _r) = running_task_multi(&repo, 2);
    let rev1 = start_item_id(&repo, &task_id, "item-1");
    let first = publish_bars(&repo, &task_id, rev1, "pub-1", "v1", vec![bar(0, 100)]);
    let rev2 = start_item_id(&repo, &task_id, "item-2");
    let cmd = command_at(&task_id, "pub-2", rev2, "item-2");
    repo.stage(&staged_raw(&cmd.version_id, vec![bar(0, 100), bar(1, 110)]))
        .unwrap();
    let c = Connection::open(&path).unwrap();
    let staged: i64 = c
        .query_row(
            "SELECT count(*) FROM staged_records WHERE version_id=?1",
            [&cmd.version_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(staged, 1, "orphaned candidate must exist before commit");

    repo.commit_range_ops(
        &cmd,
        &scope(),
        &first.version_id,
        &[bar(1, 110)],
        &[],
        &[bar(0, 100), bar(1, 110)],
        None,
    )
    .unwrap()
    .unwrap();
    let staged: i64 = c
        .query_row("SELECT count(*) FROM staged_records", [], |r| r.get(0))
        .unwrap();
    assert_eq!(staged, 0, "typed commit must consume the staged orphan");
}

#[test]
fn yearly_commit_consumes_its_staged_candidate() {
    // The yearly pipeline stages `yearly-version-…` during prepare; the
    // commit consumed it — the leaked rows in the S5 result DB came from
    // this path (one staged row per yearly event).
    let r = Repo::new();
    let mut daily_task = r.task(1, "day", "req-d");
    let (_i, pub_d, rev_d) = daily_task.item(&r.repo);
    let daily = publish_bars(
        &r.repo,
        &daily_task.task_id,
        rev_d,
        &pub_d,
        "v1",
        vec![bar(0, 100), bar(1, 110)],
    );
    let base = daily_checked(&r);
    let mut yearly_task = r.task(1, "year", "req-y");
    let candidate = mk_year_version("y-v1", vec![year_bar(2020, 100)], &daily.version_id);
    // The same write prepare_derived_yearly performs when the candidate is
    // absent (yearly_publication.rs stage-on-prepare).
    r.repo.stage(checked(&candidate).raw()).unwrap();
    let c = Connection::open(&r.path).unwrap();
    let staged: i64 = c
        .query_row(
            "SELECT count(*) FROM staged_records WHERE version_id='y-v1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(staged, 1, "yearly candidate must be staged before commit");

    commit_yearly_plan(&r, &mut yearly_task, candidate, &base, None, None, None).unwrap();
    let staged: i64 = c
        .query_row("SELECT count(*) FROM staged_records", [], |r| r.get(0))
        .unwrap();
    assert_eq!(staged, 0, "yearly commit must consume the staged row");
}

#[test]
fn yearly_replay_consumes_the_restaged_candidate() {
    // A retried yearly item stages its candidate again and the commit hits
    // the replay arm — replay must consume the re-staged row too.
    let r = Repo::new();
    let mut daily_task = r.task(1, "day", "req-d");
    let (_i, pub_d, rev_d) = daily_task.item(&r.repo);
    let daily = publish_bars(
        &r.repo,
        &daily_task.task_id,
        rev_d,
        &pub_d,
        "v1",
        vec![bar(0, 100), bar(1, 110)],
    );
    let base = daily_checked(&r);
    let mut yearly_task = r.task(1, "year", "req-y");
    let candidate = mk_year_version("y-v1", vec![year_bar(2020, 100)], &daily.version_id);
    commit_yearly_plan(
        &r,
        &mut yearly_task,
        candidate.clone(),
        &base,
        None,
        None,
        None,
    )
    .unwrap();

    // Re-stage the same candidate and re-commit the same result id —
    // commit_yearly's replay arm must also delete the row.
    r.repo.stage(checked(&candidate).raw()).unwrap();
    let c = Connection::open(&r.path).unwrap();
    commit_yearly_plan(
        &r,
        &mut yearly_task,
        candidate,
        &base,
        None,
        Some("year-req-y-pub-1"),
        Some("item-1"),
    )
    .unwrap();
    let staged: i64 = c
        .query_row("SELECT count(*) FROM staged_records", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        staged, 0,
        "replayed yearly commit must consume the staged row"
    );
}

#[test]
fn failed_commit_keeps_the_staged_row_until_gc() {
    // A rejected commit rolls back the consume: the candidate survives for
    // the retry, and only the TTL sweep reclaims it.
    let dir = tempdir();
    let (path, repo) = repo(&dir);
    let (task_id, _r) = running_task_multi(&repo, 2);
    let rev1 = start_item_id(&repo, &task_id, "item-1");
    let _first = publish_bars(&repo, &task_id, rev1, "pub-1", "v1", vec![bar(0, 100)]);
    let rev2 = start_item_id(&repo, &task_id, "item-2");
    let cmd = command_at(&task_id, "pub-2", rev2, "item-2");
    repo.stage(&staged_raw(&cmd.version_id, vec![bar(0, 100), bar(1, 110)]))
        .unwrap();

    // Stale base CAS fails → the transaction (including the consume) rolls
    // back and the staged row stays available for the next attempt.
    assert!(
        repo.commit_range_ops(
            &cmd,
            &scope(),
            "stale-base",
            &[bar(1, 110)],
            &[],
            &[bar(0, 100), bar(1, 110)],
            None,
        )
        .is_err()
    );
    let c = Connection::open(&path).unwrap();
    let staged: i64 = c
        .query_row(
            "SELECT count(*) FROM staged_records WHERE version_id=?1",
            [&cmd.version_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(staged, 1, "failed commit must keep the staged row");

    // The orphan is bounded by the TTL window: once its last stage attempt
    // ages out, the next stage() sweeps it.
    c.execute(
        "UPDATE staged_records SET created_at='2000-01-01T00:00:00+00:00'",
        [],
    )
    .unwrap();
    repo.stage(&staged_raw("v-later", vec![bar(0, 50)]))
        .unwrap();
    let (old, fresh): (i64, i64) = c
        .query_row(
            "SELECT count(*) FILTER (WHERE version_id=?1), count(*) FROM staged_records",
            [&cmd.version_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(old, 0, "expired orphan must be swept by the next stage");
    assert_eq!(fresh, 1, "the just-staged row survives the sweep");
}

#[test]
fn stage_time_gc_sweeps_only_expired_orphans() {
    // Rows younger than the TTL stay; rows whose last stage attempt is
    // older than the cutoff are reclaimed by the next stage() call. The
    // residual bound is the candidates staged within the window.
    let dir = tempdir();
    let (path, repo) = repo(&dir);
    repo.stage(&staged_raw("v-old", vec![bar(0, 50)])).unwrap();
    repo.stage(&staged_raw("v-fresh", vec![bar(0, 60)]))
        .unwrap();
    let c = Connection::open(&path).unwrap();
    let staged: i64 = c
        .query_row("SELECT count(*) FROM staged_records", [], |r| r.get(0))
        .unwrap();
    assert_eq!(staged, 2, "fresh orphans stay under the TTL");

    c.execute(
        "UPDATE staged_records SET created_at='2000-01-01T00:00:00+00:00' WHERE version_id='v-old'",
        [],
    )
    .unwrap();
    repo.stage(&staged_raw("v-next", vec![bar(0, 70)])).unwrap();
    let (old, rest): (i64, i64) = c
        .query_row(
            "SELECT count(*) FILTER (WHERE version_id='v-old'), count(*) FROM staged_records",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(old, 0);
    assert_eq!(rest, 2, "only the expired orphan is swept");

    // The same sweep is directly callable — a future cutoff reclaims all.
    let swept = repo
        .write(|c| {
            super::super::compact_repo::gc_staged(c, Utc::now() + chrono::Duration::seconds(1))
        })
        .unwrap();
    assert_eq!(swept, 2);
    let staged: i64 = c
        .query_row("SELECT count(*) FROM staged_records", [], |r| r.get(0))
        .unwrap();
    assert_eq!(staged, 0);
}

#[test]
fn full_commit_cycle_leaves_no_staged_rows() {
    // One whole update cycle on the compact backend: envelope
    // first-publish, typed append, no_change, yearly derive and yearly
    // rederive — every commit consumes its candidate in the same
    // transaction, so staged_records returns to zero.
    let r = Repo::new();
    let c = Connection::open(&r.path).unwrap();
    let staged = |c: &Connection| -> i64 {
        c.query_row("SELECT count(*) FROM staged_records", [], |r| r.get(0))
            .unwrap()
    };

    // First publish (envelope path: stages then consumes).
    let mut daily_task = r.task(4, "day", "req-d");
    let (_i, pub1, rev1) = daily_task.item(&r.repo);
    let daily = publish_bars(
        &r.repo,
        &daily_task.task_id,
        rev1,
        &pub1,
        "v1",
        vec![bar(0, 100), bar(1, 110)],
    );
    assert_eq!(staged(&c), 0, "first publish must consume its candidate");

    // Typed append: the command's version_id is the candidate id the
    // envelope fallback would have staged under — the typed commit reclaims
    // it too.
    let (_i2, pub2, rev2) = daily_task.item(&r.repo);
    let append_cmd = command_at(&daily_task.task_id, &pub2, rev2, "item-2");
    r.repo
        .stage(&staged_raw(
            &append_cmd.version_id,
            vec![bar(0, 100), bar(1, 110), bar(2, 120)],
        ))
        .unwrap();
    let head2 = r
        .repo
        .commit_range_ops(
            &append_cmd,
            &scope(),
            &daily.version_id,
            &[bar(2, 120)],
            &[],
            &[bar(0, 100), bar(1, 110), bar(2, 120)],
            None,
        )
        .unwrap()
        .unwrap();
    assert_eq!(staged(&c), 0, "typed append must consume the orphan");

    // No-change on the same head: nothing is staged, nothing is left.
    let (_i3, pub3, rev3) = daily_task.item(&r.repo);
    let nc_cmd = command_at(&daily_task.task_id, &pub3, rev3, "item-3");
    r.repo
        .commit_range_no_change_typed(
            &nc_cmd,
            &head2.version_id,
            &crate::market_update::DateRange {
                start: date(0),
                end: date(2),
            },
            None,
            None,
        )
        .unwrap();
    assert_eq!(staged(&c), 0, "no_change leaves the table untouched");

    // Yearly derive + rederive through the real prepare-stage/commit pair.
    let base = daily_checked(&r);
    let mut yearly_task = r.task(2, "year", "req-y");
    let y1 = mk_year_version(
        "y-v1",
        vec![year_bar(1970, 100)],
        &base.version().version_id,
    );
    r.repo.stage(checked(&y1).raw()).unwrap();
    commit_yearly_plan(&r, &mut yearly_task, y1, &base, None, None, None).unwrap();
    assert_eq!(staged(&c), 0, "yearly commit must consume its candidate");

    // Second daily version + rederived yearly on the new base.
    let (_i4, pub4, rev4) = daily_task.item(&r.repo);
    let append2 = command_at(&daily_task.task_id, &pub4, rev4, "item-4");
    r.repo
        .commit_range_ops(
            &append2,
            &scope(),
            &head2.version_id,
            &[bar(3, 130)],
            &[],
            &[bar(0, 100), bar(1, 110), bar(2, 120), bar(3, 130)],
            None,
        )
        .unwrap()
        .unwrap();
    let base2 = daily_checked(&r);
    let yearly_head = series::read_series(&c, SYMBOL, Period::Year)
        .unwrap()
        .unwrap()
        .version_id;
    let prior = checked(&mk_year_version(
        &yearly_head,
        vec![year_bar(1970, 100)],
        &base.version().version_id,
    ));
    let y2 = mk_year_version(
        "y-v2",
        vec![year_bar(1970, 130)],
        &base2.version().version_id,
    );
    r.repo.stage(checked(&y2).raw()).unwrap();
    commit_yearly_plan(&r, &mut yearly_task, y2, &base2, Some(prior), None, None).unwrap();
    assert_eq!(staged(&c), 0, "yearly rederive must consume its candidate");
}

// ---------------------------------------------------------------------------
// M1 PR-2: one daily item writes the raw and the forward 口径 together
// (issue-383 §9.1). Both series and both events commit in one transaction;
// the raw price fact never rewrites silently.
// ---------------------------------------------------------------------------

/// Build the pair commit for one first publish: forward bars as the 口径
/// candidate, raw bars as the `.day` fact.
fn commit_pair_first(
    repo: &super::super::compact_repo::SqliteCompactMarket,
    task_id: &str,
    item: &str,
    revision: u64,
    forward_bars: &[Bar],
    raw_bars: &[Bar],
) -> Result<crate::market_storage::Receipt, Error> {
    let raw_ev = AdjustmentEvidence::confirmed("raw", "raw .day fact");
    let raw = crate::market_data::RawBatch::new(raw_bars.to_vec(), raw_ev);
    repo.tx(|c| {
        let (version_ev, binding) = put_seed_material(c);
        let continuity = put_continuity_evidence(c, &json!({"mode":"first"}))?;
        commit_item_tx(
            c,
            &Commit {
                result_id: &format!("pair-{item}"),
                task_id,
                item_id: item,
                run_id: RUN,
                occupancy_key: &scope().occupancy_key(),
                expected_task_revision: revision,
                entity: SYMBOL,
                period: Period::Day,
                adjustment: Adjustment::Forward,
                base_version_id: None,
                change: SeriesChange::FirstPublish { bars: forward_bars },
                range_start_day: 0,
                range_end_day: forward_bars.len() as i64 - 1,
                algorithm_version: COLLECTION_ALGO,
                window_digest: None,
                source_binding_digest: Some(binding),
                evidence_digest: Some(version_ev),
                coverage_status: Some(CoverageStatus::Complete),
                metadata_changed: false,
                event_evidence_digest: continuity,
                published_at: Some(ts()),
                staged_version_id: None,
                defer_task_row: false,
            },
            Some(&raw),
        )
    })
}

fn series_closes(
    repo: &super::super::compact_repo::SqliteCompactMarket,
    adjustment: Adjustment,
) -> Option<Vec<i64>> {
    repo.read(|c| {
        Ok(
            series::read_series_adjusted(c, SYMBOL, Period::Day, adjustment)?.map(|row| {
                series::all_bars(c, row.series_key)
                    .unwrap()
                    .iter()
                    .map(|bar| bar.close_units)
                    .collect()
            }),
        )
    })
    .unwrap()
}

#[test]
fn daily_item_commits_both_口径_series_and_events_in_one_transaction() {
    let dir = tempdir();
    let (_path, repo) = repo(&dir);
    let (task_id, revision) = running_task(&repo);
    let forward_bars = vec![bar(0, 753), bar(1, 1131)];
    let raw_bars = vec![bar(0, 1129), bar(1, 1131)];
    let receipt =
        commit_pair_first(&repo, &task_id, ITEM, revision, &forward_bars, &raw_bars).unwrap();
    assert_eq!(
        series_closes(&repo, Adjustment::Forward),
        Some(vec![753, 1131])
    );
    assert_eq!(
        series_closes(&repo, Adjustment::Raw),
        Some(vec![1129, 1131])
    );
    let (forward_event, raw_event) = repo
        .read(|c| {
            Ok((
                series::event_by_task_item(c, &task_id, ITEM, Adjustment::Forward)?.unwrap(),
                series::event_by_task_item(c, &task_id, ITEM, Adjustment::Raw)?.unwrap(),
            ))
        })
        .unwrap();
    assert_eq!(forward_event.kind, "first_publish");
    assert_eq!(raw_event.kind, "first_publish");
    assert_eq!(
        raw_event.result_id,
        format!("{}:raw", forward_event.result_id)
    );
    assert_eq!(raw_event.task_revision, forward_event.task_revision);
    // The item finishes exactly once and the MCP-visible receipt is the
    // forward 口径.
    let task = repo.task(&task_id).unwrap();
    assert!(task.items[0].outcome.as_ref().is_some_and(|o| o.success));
    assert_eq!(task.task.status, task_control::Status::Succeeded);
    let forward_head = repo
        .read(|c| series::read_series_adjusted(c, SYMBOL, Period::Day, Adjustment::Forward))
        .unwrap()
        .unwrap();
    assert_eq!(receipt.version_id, forward_head.version_id);
    let raw_head = repo
        .read(|c| series::read_series_adjusted(c, SYMBOL, Period::Day, Adjustment::Raw))
        .unwrap()
        .unwrap();
    assert_ne!(raw_head.version_id, forward_head.version_id);
    // The raw version evidence names the raw 口径; the forward one stays qfq.
    for (adjustment, value) in [(Adjustment::Forward, "qfq"), (Adjustment::Raw, "raw")] {
        let evidence = repo
            .read(|c| {
                let row =
                    series::read_series_adjusted(c, SYMBOL, Period::Day, adjustment)?.unwrap();
                let (kind, canonical) = evidence_store::read_evidence(c, &row.evidence_digest)?;
                assert_eq!(kind, EvidenceKind::Version);
                Ok(
                    serde_json::from_str::<crate::compact_market::VersionEvidence>(&canonical)
                        .unwrap(),
                )
            })
            .unwrap();
        assert_eq!(evidence.adjustment_evidence.value.as_deref(), Some(value));
    }
}

#[test]
fn daily_item_rolls_back_both_series_when_the_raw_side_fails() {
    let dir = tempdir();
    let (_path, repo) = repo(&dir);
    let (task_id, revision) = running_task(&repo);
    let forward_bars = vec![bar(0, 753), bar(1, 1131)];
    // The raw batch omits the forward date 1 — the pair cannot be aligned.
    let raw_bars = vec![bar(0, 1129)];
    let err =
        commit_pair_first(&repo, &task_id, ITEM, revision, &forward_bars, &raw_bars).unwrap_err();
    assert!(matches!(err, Error::Integrity(_)), "{err:?}");
    assert_eq!(series_closes(&repo, Adjustment::Forward), None);
    assert_eq!(series_closes(&repo, Adjustment::Raw), None);
    assert!(
        repo.read(|c| series::event_by_task_item(c, &task_id, ITEM, Adjustment::Forward))
            .unwrap()
            .is_none()
    );
    assert!(
        repo.read(|c| series::event_by_task_item(c, &task_id, ITEM, Adjustment::Raw))
            .unwrap()
            .is_none()
    );
    let task = repo.task(&task_id).unwrap();
    assert!(task.items[0].outcome.is_none());
    assert_eq!(task.task.status, task_control::Status::Running);
}

#[test]
fn future_event_rewrites_only_the_forward_口径() {
    let dir = tempdir();
    let (_path, repo) = repo(&dir);
    let (first_task, first_revision) = running_task(&repo);
    let forward_bars = vec![bar(0, 753), bar(1, 1131)];
    let raw_bars = vec![bar(0, 1129), bar(1, 1131)];
    let first = commit_pair_first(
        &repo,
        &first_task,
        ITEM,
        first_revision,
        &forward_bars,
        &raw_bars,
    )
    .unwrap();
    let raw_head_before = repo
        .read(|c| series::read_series_adjusted(c, SYMBOL, Period::Day, Adjustment::Raw))
        .unwrap()
        .unwrap();
    // A future-effective corporate action reprices the derived history: the
    // forward bars change, the raw `.day` facts do not.
    let (second_task, second_revision) = running_task_for(&repo, "req-2", "item-2");
    let rewritten_forward = vec![bar(0, 800), bar(1, 1131)];
    let raw_ev = AdjustmentEvidence::confirmed("raw", "raw .day fact");
    let raw = crate::market_data::RawBatch::new(raw_bars.clone(), raw_ev);
    repo.tx(|c| {
        let (version_ev, binding) = put_seed_material(c);
        let continuity = put_continuity_evidence(c, &json!({"mode":"full"}))?;
        commit_item_tx(
            c,
            &Commit {
                result_id: "pair-item-2",
                task_id: &second_task,
                item_id: "item-2",
                run_id: RUN,
                occupancy_key: &scope().occupancy_key(),
                expected_task_revision: second_revision,
                entity: SYMBOL,
                period: Period::Day,
                adjustment: Adjustment::Forward,
                base_version_id: Some(&first.version_id),
                change: SeriesChange::Replace {
                    bars: &rewritten_forward,
                },
                range_start_day: 0,
                range_end_day: 1,
                algorithm_version: UPDATE_ALGO,
                window_digest: None,
                source_binding_digest: Some(binding),
                evidence_digest: Some(version_ev),
                coverage_status: Some(CoverageStatus::Complete),
                metadata_changed: false,
                event_evidence_digest: continuity,
                published_at: None,
                staged_version_id: None,
                defer_task_row: false,
            },
            Some(&raw),
        )
    })
    .unwrap();
    assert_eq!(
        series_closes(&repo, Adjustment::Forward),
        Some(vec![800, 1131])
    );
    assert_eq!(
        series_closes(&repo, Adjustment::Raw),
        Some(vec![1129, 1131])
    );
    let raw_head_after = repo
        .read(|c| series::read_series_adjusted(c, SYMBOL, Period::Day, Adjustment::Raw))
        .unwrap()
        .unwrap();
    assert_eq!(
        raw_head_after.version_id, raw_head_before.version_id,
        "the raw content did not change"
    );
    let (forward_event, raw_event) = repo
        .read(|c| {
            Ok((
                series::event_by_task_item(c, &second_task, "item-2", Adjustment::Forward)?
                    .unwrap(),
                series::event_by_task_item(c, &second_task, "item-2", Adjustment::Raw)?.unwrap(),
            ))
        })
        .unwrap();
    assert_eq!(forward_event.kind, "replace");
    assert_eq!(raw_event.kind, "no_change");
}

#[test]
fn raw_series_rewrite_without_evidence_fails_closed() {
    let dir = tempdir();
    let (_path, repo) = repo(&dir);
    let (first_task, first_revision) = running_task(&repo);
    let forward_bars = vec![bar(0, 753), bar(1, 1131)];
    let raw_bars = vec![bar(0, 1129), bar(1, 1131)];
    let first = commit_pair_first(
        &repo,
        &first_task,
        ITEM,
        first_revision,
        &forward_bars,
        &raw_bars,
    )
    .unwrap();
    let (second_task, second_revision) = running_task_for(&repo, "req-2", "item-2");
    // A rewritten raw price on an already-stored date is a basis conflict
    // with no revision evidence: the pair must fail closed.
    let changed_raw = vec![bar(0, 1100), bar(1, 1131)];
    let raw_ev = AdjustmentEvidence::confirmed("raw", "raw .day fact");
    let raw = crate::market_data::RawBatch::new(changed_raw, raw_ev);
    let err = repo
        .tx(|c| {
            let (version_ev, binding) = put_seed_material(c);
            let continuity = put_continuity_evidence(c, &json!({"mode":"full"}))?;
            commit_item_tx(
                c,
                &Commit {
                    result_id: "pair-item-2",
                    task_id: &second_task,
                    item_id: "item-2",
                    run_id: RUN,
                    occupancy_key: &scope().occupancy_key(),
                    expected_task_revision: second_revision,
                    entity: SYMBOL,
                    period: Period::Day,
                    adjustment: Adjustment::Forward,
                    base_version_id: Some(&first.version_id),
                    change: SeriesChange::Replace {
                        bars: &forward_bars,
                    },
                    range_start_day: 0,
                    range_end_day: 1,
                    algorithm_version: UPDATE_ALGO,
                    window_digest: None,
                    source_binding_digest: Some(binding),
                    evidence_digest: Some(version_ev),
                    coverage_status: Some(CoverageStatus::Complete),
                    metadata_changed: false,
                    event_evidence_digest: continuity,
                    published_at: None,
                    staged_version_id: None,
                    defer_task_row: false,
                },
                Some(&raw),
            )
        })
        .unwrap_err();
    assert!(matches!(err, Error::Integrity(_)), "{err:?}");
    // The forward rewrite rolled back with the raw failure; both heads and
    // the raw bars are untouched.
    assert_eq!(
        series_closes(&repo, Adjustment::Forward),
        Some(vec![753, 1131])
    );
    assert_eq!(
        series_closes(&repo, Adjustment::Raw),
        Some(vec![1129, 1131])
    );
    assert_eq!(
        repo.read(|c| series::read_series_adjusted(c, SYMBOL, Period::Day, Adjustment::Forward))
            .unwrap()
            .unwrap()
            .version_id,
        first.version_id
    );
}
