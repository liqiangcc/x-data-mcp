use std::collections::BTreeSet;
use std::path::PathBuf;

use serde_json::json;

use super::*;
use crate::market_data::{
    AdjustmentEvidence, CoverageEvidence, EvidenceValue, UnitCurrencyEvidence,
};
use crate::market_storage::PublishIntent;
use crate::publish::ValidationSummary;
use crate::task_control::{CommandKey, ItemSpec};

fn tempdir() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/dev/shm")
        .unwrap()
}

fn repo(dir: &tempfile::TempDir) -> (PathBuf, SqliteCompactMarket) {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.path().join("market.db");
    crate::sqlite_tasks::SqliteTasks::new(&path)
        .initialize19()
        .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let repo = SqliteCompactMarket::open(&path).unwrap();
    (path, repo)
}

fn ts() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-09-21T00:00:00+00:00")
        .unwrap()
        .with_timezone(&Utc)
}

fn date(day: i64) -> NaiveDate {
    compact_market::epoch_date(day).unwrap()
}

fn bar(day: i64, cents: i64) -> Bar {
    CompactBar {
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

fn mk_version(version_id: &str, bars: Vec<Bar>) -> EffectiveVersion {
    let mut version = EffectiveVersion {
        version_id: version_id.into(),
        symbol: "sh.600006".into(),
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
    let (records, validation) =
        crate::publish::validate_and_deduplicate(&mk_batch(&version)).unwrap();
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
            adapter_version: "effective-publish".into(),
            yearly_derivation: version.yearly_derivation.clone(),
            snapshot_inputs: None,
        },
        coverage: CoverageEvidence::complete("full history verified"),
        unit_currency: units(),
        adjustment_evidence: AdjustmentEvidence::confirmed("qfq", "bound"),
    }
}

fn source(generation: &str) -> Value {
    json!({
        "generation_id": generation,
        "generation_manifest_sha256": "ab".repeat(32),
        "day_member_sha256": "cd".repeat(32),
        "security_event_sha256": "ef".repeat(32),
        "generation_evidence": {
            "snapshot_inputs": [{
                "role": "daily",
                "member": "sh.600006.day",
                "archive_sha256": "12".repeat(32),
                "member_sha256": "cd".repeat(32)
            }]
        }
    })
}

fn envelope(version: &EffectiveVersion, source: Value, parent: Option<&str>) -> Value {
    let mut raw = serde_json::to_value(version).unwrap();
    raw["batch"] = serde_json::to_value(mk_batch(version)).unwrap();
    raw["compact_source"] = source;
    if let Some(parent) = parent {
        raw["compact_parent_version_id"] = json!(parent);
    }
    raw["integrity_sha256"] = json!(compat::legacy_digest(&raw).unwrap());
    raw
}

const ITEM: &str = "item-1";
const RUN: &str = "run";

fn accept_task(repo: &SqliteCompactMarket) -> String {
    let scope = task_control::legacy::scope_id("sh.600006", "day");
    let work = FrozenWork {
        items: vec![ItemSpec {
            item_id: ITEM.into(),
            target: json!({"symbol": "sh.600006", "period": "day"}),
            progress_unit: "bars".into(),
            frozen_inputs: json!({}),
            scope_keys: BTreeSet::from([scope.clone()]),
        }],
        scopes: BTreeSet::from([scope]),
        shared_evidence: None,
    };
    let command = Command {
        key: CommandKey {
            operation: "collect_data".into(),
            dataset: "market.daily".into(),
            contract_version: "v1".into(),
        },
        intent: json!({}),
    };
    let acceptance = repo.accept("req-1", &command, &work, RUN, None).unwrap();
    match acceptance.record.outcome {
        task_control::RequestOutcome::Task(snapshot) => snapshot.task.task_id.clone(),
        _ => panic!("expected task outcome"),
    }
}

fn claim(repo: &SqliteCompactMarket, task_id: &str) -> u64 {
    let revision = repo.task(task_id).unwrap().task.state_revision;
    repo.transition(task_id, revision, RUN, Change::Claim)
        .unwrap()
        .task
        .state_revision
}

fn start_item(repo: &SqliteCompactMarket, task_id: &str) -> u64 {
    let revision = claim(repo, task_id);
    repo.transition(task_id, revision, RUN, Change::StartItem(ITEM.into()))
        .unwrap()
        .task
        .state_revision
}

fn collect_command(
    task_id: &str,
    publication_id: &str,
    version_id: &str,
    revision: u64,
) -> Publication {
    Publication {
        publication_id: publication_id.into(),
        version_id: version_id.into(),
        task_id: task_id.into(),
        item_id: ITEM.into(),
        run_id: RUN.into(),
        expected_revision: revision,
        intent: PublishIntent::Daily {
            expected_head: None,
        },
    }
}

#[test]
fn collection_publish_end_to_end() {
    let dir = tempdir();
    let (_, repo) = repo(&dir);
    let task_id = accept_task(&repo);
    let revision = start_item(&repo, &task_id);

    let version = mk_version("v1", vec![bar(0, 100), bar(1, 110)]);
    repo.stage(&envelope(&version, source("gen-1"), None))
        .unwrap();
    let candidate = repo.candidate("v1").unwrap();
    assert_eq!(candidate.version.records.len(), 2);
    assert_eq!(
        candidate.raw()["compact_source"]["generation_id"],
        json!("gen-1")
    );
    // Deterministic synthesis: re-reading yields byte-identical raw.
    let again = repo.candidate("v1").unwrap();
    assert_eq!(candidate.raw(), again.raw());

    repo.mark_validated(&candidate).unwrap();
    let plan = PublishPlan {
        command: collect_command(&task_id, "pub-1", "v1", revision),
        record: candidate.clone(),
        required_heads: vec![(Scope::of(&candidate.version), None)],
        promote: None,
        base_digest: None,
    };
    let receipt = repo.commit(&plan).unwrap();
    assert_eq!(receipt.version_id, "v1");

    let fixed = repo.fixed("v1").unwrap();
    assert_eq!(fixed.version.records.len(), 2);
    // `published_at` is stamped at commit; every other fact is identical.
    assert_eq!(fixed.version.records, candidate.version.records);
    assert_eq!(
        fixed.raw()["compact_source"],
        candidate.raw()["compact_source"]
    );

    let scope = Scope::of(&fixed.version);
    let page = repo
        .range(
            &scope,
            None,
            &Range {
                start: date(0),
                end: date(10),
                after: None,
                limit: 10,
            },
        )
        .unwrap();
    assert_eq!(page.rows.len(), 2);
    // The page digest is the synthesized legacy digest of the fixed version
    // (published_at stamped at commit differs from the staged projection).
    assert!(crate::hash::is_sha256(&page.integrity_sha256));

    let versions = repo.versions(&scope).unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].version_id, "v1");
    assert!(repo.receipt("pub-1").unwrap().is_some());

    // Idempotent replay returns the same receipt without rewriting.
    let replay = repo.commit(&plan).unwrap();
    assert_eq!(replay, receipt);

    let binding = repo.task_item_binding(&task_id, ITEM).unwrap().unwrap();
    assert_eq!(binding.item.stage, task_control::Stage::Finished);
}

#[test]
fn append_update_promotes_head_and_preserves_base() {
    let dir = tempdir();
    let (_, repo) = repo(&dir);
    let task_id = accept_task(&repo);
    let revision = start_item(&repo, &task_id);

    let base = mk_version("v1", vec![bar(0, 100), bar(1, 110)]);
    repo.stage(&envelope(&base, source("gen-1"), None)).unwrap();
    let base_checked = repo.candidate("v1").unwrap();
    repo.mark_validated(&base_checked).unwrap();
    repo.commit(&PublishPlan {
        command: collect_command(&task_id, "pub-1", "v1", revision),
        record: base_checked.clone(),
        required_heads: vec![(Scope::of(&base_checked.version), None)],
        promote: None,
        base_digest: None,
    })
    .unwrap();

    // Second task for the update item.
    let task_id2 = {
        let scope = task_control::legacy::scope_id("sh.600006", "day");
        let work = FrozenWork {
            items: vec![ItemSpec {
                item_id: ITEM.into(),
                target: json!({"symbol": "sh.600006", "period": "day"}),
                progress_unit: "bars".into(),
                frozen_inputs: json!({}),
                scope_keys: BTreeSet::from([scope.clone()]),
            }],
            scopes: BTreeSet::from([scope]),
            shared_evidence: None,
        };
        let command = Command {
            key: CommandKey {
                operation: "update_data".into(),
                dataset: "market.daily".into(),
                contract_version: "v1".into(),
            },
            intent: json!({}),
        };
        let acceptance = repo.accept("req-2", &command, &work, RUN, None).unwrap();
        match acceptance.record.outcome {
            task_control::RequestOutcome::Task(snapshot) => snapshot.task.task_id.clone(),
            _ => panic!("expected task outcome"),
        }
    };
    let revision2 = start_item(&repo, &task_id2);

    let updated = mk_version("v2", vec![bar(0, 100), bar(1, 110), bar(2, 120)]);
    repo.stage(&envelope(&updated, source("gen-2"), Some("v1")))
        .unwrap();
    let checked = repo.candidate("v2").unwrap();
    assert_eq!(checked.raw()["compact_parent_version_id"], json!("v1"));
    repo.mark_validated(&checked).unwrap();

    let receipt = repo
        .commit(&PublishPlan {
            command: Publication {
                publication_id: "upd-1".into(),
                version_id: "v2".into(),
                task_id: task_id2.clone(),
                item_id: ITEM.into(),
                run_id: RUN.into(),
                expected_revision: revision2,
                intent: PublishIntent::Daily {
                    expected_head: Some("v1".into()),
                },
            },
            record: checked.clone(),
            required_heads: vec![(Scope::of(&checked.version), Some("v1".into()))],
            promote: None,
            base_digest: None,
        })
        .unwrap();
    assert_eq!(receipt.version_id, "v2");

    let head = repo
        .range(
            &Scope::of(&checked.version),
            None,
            &Range {
                start: date(0),
                end: date(10),
                after: None,
                limit: 10,
            },
        )
        .unwrap();
    assert_eq!(head.version.version_id, "v2");
    assert_eq!(head.rows.len(), 3);

    // The append stored ONLY the changed bar: delta segments never copy the
    // parent's tail chunk. 2 base bars + 1 new bar = 3 physical bar rows.
    let bar_rows: i64 = repo
        .read(|c| {
            c.query_row("SELECT count(*) FROM segment_bars", [], |r| r.get(0))
                .map_err(storage)
        })
        .unwrap();
    assert_eq!(bar_rows, 3);

    // Base version remains immutable and independently readable.
    assert_eq!(repo.fixed("v1").unwrap().version.records.len(), 2);
    let receipt = repo.range_receipt("upd-1").unwrap().unwrap();
    assert_eq!(receipt.version_id, "v2");
    let item_receipt = repo.item_receipt(&task_id2, ITEM).unwrap().unwrap();
    assert_eq!(item_receipt.version_id, "v2");
}

#[test]
fn no_change_range_commit_writes_only_typed_result() {
    let dir = tempdir();
    let (path, repo) = repo(&dir);
    let task_id = accept_task(&repo);
    let revision = start_item(&repo, &task_id);

    let base = mk_version("v1", vec![bar(0, 100), bar(1, 110)]);
    repo.stage(&envelope(&base, source("gen-1"), None)).unwrap();
    let base_checked = repo.candidate("v1").unwrap();
    repo.mark_validated(&base_checked).unwrap();
    repo.commit(&PublishPlan {
        command: collect_command(&task_id, "pub-1", "v1", revision),
        record: base_checked.clone(),
        required_heads: vec![(Scope::of(&base_checked.version), None)],
        promote: None,
        base_digest: None,
    })
    .unwrap();

    // New update task for the no-change range commit.
    let scope_id = task_control::legacy::scope_id("sh.600006", "day");
    let work = FrozenWork {
        items: vec![ItemSpec {
            item_id: "item-2".into(),
            target: json!({"symbol": "sh.600006", "period": "day"}),
            progress_unit: "bars".into(),
            frozen_inputs: json!({}),
            scope_keys: BTreeSet::from([scope_id.clone()]),
        }],
        scopes: BTreeSet::from([scope_id]),
        shared_evidence: None,
    };
    let command = Command {
        key: CommandKey {
            operation: "update_data".into(),
            dataset: "market.daily".into(),
            contract_version: "v1".into(),
        },
        intent: json!({}),
    };
    let acceptance = repo.accept("req-2", &command, &work, RUN, None).unwrap();
    let task2 = match acceptance.record.outcome {
        task_control::RequestOutcome::Task(snapshot) => snapshot.task.task_id.clone(),
        _ => panic!("expected task outcome"),
    };
    // The first task owns the scope until it finishes; the no-change commit
    // finishes it, so bind the update task after the commit would deadlock.
    // Instead the update task is accepted only after task-1's item finished.
    let claim2 = claim(&repo, &task2);
    let revision2 = repo
        .transition(&task2, claim2, RUN, Change::StartItem("item-2".into()))
        .unwrap()
        .task
        .state_revision;

    let counts = |repo: &SqliteCompactMarket| -> Vec<i64> {
        repo.read(|c| {
            [
                "data_versions",
                "bar_segments",
                "segment_bars",
                "market_update_results",
            ]
            .iter()
            .map(|table| {
                c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                    .map_err(storage)
            })
            .collect()
        })
        .unwrap()
    };
    let before = counts(&repo);

    let plan = RangePlan {
        publication: PublishPlan {
            command: Publication {
                publication_id: "upd-nochange".into(),
                version_id: "v1".into(),
                task_id: task2.clone(),
                item_id: "item-2".into(),
                run_id: RUN.into(),
                expected_revision: revision2,
                intent: PublishIntent::Daily {
                    expected_head: Some("v1".into()),
                },
            },
            record: base_checked.clone(),
            required_heads: vec![(Scope::of(&base_checked.version), Some("v1".into()))],
            promote: None,
            base_digest: None,
        },
        base: base_checked.clone(),
        no_change: true,
        audit: json!({
            "algorithm_version": "local-range-update-v1",
            "range": {"start": "2026-09-01", "end": "2026-09-21"},
            "evidence": {"same_qfq_basis_ref": "basis-1", "revision_ref": "rev-1"}
        }),
        audit_payloads: Vec::new(),
    };
    let receipt = repo.commit_range(&plan).unwrap();
    assert_eq!(receipt.version_id, "v1");

    let after = counts(&repo);
    // Only one typed market_update_results row is added — no bars, segments,
    // or versions are duplicated for a no-change update.
    assert_eq!(&after[..3], &before[..3]);
    assert_eq!(after[3], before[3] + 1);
    assert!(repo.range_receipt("upd-nochange").unwrap().is_some());
    assert!(path.exists());
}

#[test]
fn legacy_adapters_reject_schema19() {
    let dir = tempdir();
    let (path, _) = repo(&dir);
    let legacy =
        crate::sqlite_market::SqliteMarket::new(crate::sqlite_tasks::SqliteTasks::new(&path));
    assert!(legacy.task("missing").is_err());
    assert!(
        crate::sqlite_tasks::SqliteTasks::new(&path)
            .upgrade()
            .is_err()
    );
}

#[test]
fn backend_dispatch_and_compact_supply_surface() {
    use crate::data_supply::query::QueryPlan;
    use crate::data_supply::wire::{
        ContractVersion, Dataset, Exchange, Filter, Query, Security, Selection, VersionChoice,
        VersionsQuery,
    };
    use crate::data_supply::{Access, query::ReadRepository};
    use crate::sqlite_market::backend::MarketBackend;
    use crate::sqlite_supply::SqliteSupply;

    let dir = tempdir();
    let (_path, repo) = repo(&dir);
    let task_id = accept_task(&repo);
    let revision = start_item(&repo, &task_id);
    let version = mk_version("v1", vec![bar(0, 100), bar(1, 110)]);
    repo.stage(&envelope(&version, source("gen-1"), None))
        .unwrap();
    let candidate = repo.candidate("v1").unwrap();
    repo.mark_validated(&candidate).unwrap();
    repo.commit(&PublishPlan {
        command: collect_command(&task_id, "pub-1", "v1", revision),
        record: candidate.clone(),
        required_heads: vec![(Scope::of(&candidate.version), None)],
        promote: None,
        base_digest: None,
    })
    .unwrap();

    // Schema-19 dispatch selects the compact backend. The service lease is
    // single-owner, so the first adapter is dropped before reopening.
    let tasks = repo.tasks();
    drop(repo);
    let backend = MarketBackend::open(&tasks).unwrap();
    assert!(matches!(backend, MarketBackend::Compact(_)));
    assert!(backend.task(&task_id).is_ok());
    assert_eq!(backend.tasks().path(), tasks.path());

    // The compact read surface serves versions + query + verify.
    let supply = SqliteSupply {
        market: backend.clone(),
    };
    let access = Access::local();
    let entity = Security {
        exchange: Exchange::Sh,
        code: "600006".into(),
    };
    let infos = supply
        .versions(
            &VersionsQuery {
                dataset: Dataset::Daily,
                contract_version: ContractVersion::V1,
                selection: Selection::Single {
                    entity: entity.clone(),
                },
                page_size: 10,
                cursor: None,
            },
            &access,
        )
        .unwrap();
    assert_eq!(infos.len(), 1);
    assert_eq!(infos[0].version_ref.version_id, "v1");

    let plan = QueryPlan::new(
        Query {
            dataset: Dataset::Daily,
            contract_version: ContractVersion::V1,
            selection: Selection::Single { entity },
            filter: Filter::Daily {
                start_date: date(0),
                end_date: date(10),
            },
            version: VersionChoice::LatestPerEntity,
            page_size: 10,
            cursor: None,
        },
        &access,
    )
    .unwrap();
    let snapshot = supply.query(&plan, &access).unwrap();
    assert_eq!(snapshot.rows.len(), 2);

    // verify_versions round-trips the same snapshot digests.
    supply.verify(&[infos[0].version_ref.clone()]).unwrap();

    // A second fresh v18 database still selects the legacy backend.
    let legacy_dir = tempdir();
    let legacy_path = legacy_dir.path().join("legacy.db");
    let legacy_tasks = SqliteTasks::new(legacy_path);
    legacy_tasks.initialize().unwrap();
    assert!(matches!(
        MarketBackend::open(&legacy_tasks).unwrap(),
        MarketBackend::Legacy(_)
    ));
}

#[test]
fn ops_range_commit_stages_delta_binds_leaf_and_replays() {
    use crate::market_update::publication::RangeRepository;

    let dir = tempdir();
    let (_, repo) = repo(&dir);
    let task_id = accept_task(&repo);
    let revision = start_item(&repo, &task_id);

    let base = mk_version("v1", vec![bar(0, 100), bar(1, 110)]);
    repo.stage(&envelope(&base, source("gen-1"), None)).unwrap();
    let base_checked = repo.candidate("v1").unwrap();
    repo.mark_validated(&base_checked).unwrap();
    repo.commit(&PublishPlan {
        command: collect_command(&task_id, "pub-1", "v1", revision),
        record: base_checked.clone(),
        required_heads: vec![(Scope::of(&base_checked.version), None)],
        promote: None,
        base_digest: None,
    })
    .unwrap();

    let task_id2 = {
        let scope = task_control::legacy::scope_id("sh.600006", "day");
        let work = FrozenWork {
            items: vec![ItemSpec {
                item_id: ITEM.into(),
                target: json!({"symbol": "sh.600006", "period": "day"}),
                progress_unit: "bars".into(),
                frozen_inputs: json!({}),
                scope_keys: BTreeSet::from([scope.clone()]),
            }],
            scopes: BTreeSet::from([scope]),
            shared_evidence: None,
        };
        let command = Command {
            key: CommandKey {
                operation: "update_data".into(),
                dataset: "market.daily".into(),
                contract_version: "v1".into(),
            },
            intent: json!({}),
        };
        match repo
            .accept("req-2", &command, &work, RUN, None)
            .unwrap()
            .record
            .outcome
        {
            task_control::RequestOutcome::Task(snapshot) => snapshot.task.task_id.clone(),
            _ => panic!("expected task outcome"),
        }
    };
    let revision2 = start_item(&repo, &task_id2);
    let scope = Scope::of(&base_checked.version);
    let approved = vec![bar(0, 100), bar(1, 110), bar(2, 120)];
    let command = Publication {
        publication_id: "upd-ops-1".into(),
        version_id: "v2".into(),
        task_id: task_id2.clone(),
        item_id: ITEM.into(),
        run_id: RUN.into(),
        expected_revision: revision2,
        intent: PublishIntent::Daily {
            expected_head: Some("v1".into()),
        },
    };

    // Leaf mismatch against the approved merge fails closed before commit.
    let wrong = vec![bar(0, 100), bar(1, 110), bar(2, 999)];
    assert!(
        repo.commit_range_ops(&command, &scope, "v1", &[bar(2, 120)], &[], &wrong)
            .is_err()
    );

    // Correct ops commit: staged delta binds leaf == approved merge.
    let receipt = repo
        .commit_range_ops(&command, &scope, "v1", &[bar(2, 120)], &[], &approved)
        .unwrap()
        .expect("compact backend supports ops commit");
    assert_eq!(receipt.version_id, "v2");

    let page = repo
        .range(
            &scope,
            None,
            &Range {
                start: date(0),
                end: date(10),
                after: None,
                limit: 10,
            },
        )
        .unwrap();
    assert_eq!(page.version.version_id, "v2");
    assert_eq!(page.rows.len(), 3);

    // The delta stored only the changed bar row.
    let bar_rows: i64 = repo
        .read(|c| {
            c.query_row("SELECT count(*) FROM segment_bars", [], |r| r.get(0))
                .map_err(storage)
        })
        .unwrap();
    assert_eq!(bar_rows, 3);

    // Replay: item receipt is already committed; a fresh commit_range_ops on
    // a new item id would restage harmlessly, but the committed item's
    // receipt is authoritative.
    let replay = repo.item_receipt(&task_id2, ITEM).unwrap().unwrap();
    assert_eq!(replay, receipt);
}

fn collection_candidate_for(
    version: &EffectiveVersion,
    task_id: &str,
    run: &str,
) -> CollectionCandidate {
    CollectionCandidate {
        task_id: task_id.into(),
        item_id: ITEM.into(),
        publication_id: "pub-1".into(),
        version_id: version.version_id.clone(),
        run_id: run.into(),
        symbol: version.symbol.clone(),
        source: version.source.clone(),
        bars: version.records.clone(),
        coverage: CoverageEvidence::complete("full history verified"),
        unit_currency: units(),
        adjustment_evidence: AdjustmentEvidence::confirmed("qfq", "bound"),
        published_at: version.published_at,
        generation_id: "gen-1".into(),
        generation_manifest_sha256: "ab".repeat(32),
        day_member_sha256: "cd".repeat(32),
        security_event_sha256: "ef".repeat(32),
        generation_evidence: json!({
            "snapshot_inputs": [{
                "role": "daily",
                "member": "sh.600006.day",
                "archive_sha256": "12".repeat(32),
                "member_sha256": "cd".repeat(32)
            }]
        }),
    }
}

#[test]
fn typed_collection_batch_is_byte_equal_to_envelope_path() {
    // Envelope path on repo1.
    let dir1 = tempdir();
    let (_, repo1) = repo(&dir1);
    let task_id = accept_task(&repo1);
    let revision = start_item(&repo1, &task_id);
    let version = mk_version("v1", vec![bar(0, 100), bar(1, 110), bar(2, 120)]);
    repo1
        .stage(&envelope(&version, source("gen-1"), None))
        .unwrap();
    let checked = repo1.candidate("v1").unwrap();
    repo1.mark_validated(&checked).unwrap();
    repo1
        .commit(&PublishPlan {
            command: collect_command(&task_id, "pub-1", "v1", revision),
            record: checked.clone(),
            required_heads: vec![(Scope::of(&checked.version), None)],
            promote: None,
            base_digest: None,
        })
        .unwrap();

    // Typed batch path on repo2 — same task inputs → same task_id.
    let dir2 = tempdir();
    let (_, repo2) = repo(&dir2);
    let task_id2 = accept_task(&repo2);
    let _ = (task_id, task_id2.clone());
    let results = repo2
        .commit_collection_batch(
            &task_id2,
            &[collection_candidate_for(&version, &task_id2, RUN)],
        )
        .unwrap();
    let receipt = results.into_iter().next().unwrap().unwrap();
    assert_eq!(receipt.version_id, "v1");

    // Every stored digest-bearing row must be byte-identical except
    // task-bound fields (task ids are per-database) and the version
    // publish stamp (envelope commits stamp at commit time).
    let dump = |repo: &SqliteCompactMarket, table: &str| {
        repo.read(|c| {
            let mut stmt = c.prepare(&format!("SELECT * FROM {table}")).unwrap();
            let names: Vec<String> = stmt.column_names().iter().map(|n| n.to_string()).collect();
            let mut rows = Vec::new();
            let mut iter = stmt.query([]).unwrap();
            while let Some(row) = iter.next().unwrap() {
                let mut obj = serde_json::Map::new();
                for (i, name) in names.iter().enumerate() {
                    let value: rusqlite::types::Value = row.get(i).unwrap();
                    obj.insert(
                        name.clone(),
                        match value {
                            rusqlite::types::Value::Blob(b) => json!(hex_bytes(&b)),
                            rusqlite::types::Value::Integer(n) => json!(n),
                            rusqlite::types::Value::Text(t) => json!(t),
                            rusqlite::types::Value::Real(f) => json!(f),
                            rusqlite::types::Value::Null => Value::Null,
                        },
                    );
                }
                rows.push(Value::Object(obj));
            }
            Ok(json!(rows))
        })
        .unwrap()
    };
    for table in [
        "bar_segments",
        "segment_bars",
        "version_checkpoint_clips",
        "source_bindings",
        "evidence_bundles",
        "effective_heads",
    ] {
        let a = dump(&repo1, table);
        let b = dump(&repo2, table);
        assert_eq!(a, b, "table {table} differs between paths");
    }
    // publication_results / task_publications: identical except the
    // task-bound fields and the digest that binds them.
    let (pa, pb) = (
        dump(&repo1, "publication_results"),
        dump(&repo2, "publication_results"),
    );
    for (key, value) in pa[0].as_object().unwrap() {
        if key == "task_id" || key == "result_digest" {
            continue;
        }
        assert_eq!(value, &pb[0][key], "publication_results.{key} differs");
    }
    let (ta, tb) = (
        dump(&repo1, "task_publications"),
        dump(&repo2, "task_publications"),
    );
    for (key, value) in ta[0].as_object().unwrap() {
        if key == "task_id" {
            continue;
        }
        assert_eq!(value, &tb[0][key], "task_publications.{key} differs");
    }
    // The result digest is byte-equal under a shared task id.
    let digest_for = |repo: &SqliteCompactMarket, task: &str| {
        repo.read(|c| {
            let stored = compact_results::read_publication(c, "pub-1")?;
            let identity = compact_results::publication_identity_with_task(&stored, task);
            compact_market::publication_result_digest(&identity)
                .map(|d| hex_bytes(&d))
                .map_err(integrity)
        })
        .unwrap()
    };
    assert_eq!(digest_for(&repo1, "t"), digest_for(&repo2, "t"));
    // data_versions: all fields equal except published_at/integrity_digest.
    let rows_a = dump(&repo1, "data_versions");
    let rows_b = dump(&repo2, "data_versions");
    assert_eq!(rows_a.as_array().unwrap().len(), 1);
    let (va, vb) = (&rows_a[0], &rows_b[0]);
    for (key, value) in va.as_object().unwrap() {
        if key == "published_at" || key == "integrity_digest" {
            continue;
        }
        assert_eq!(value, &vb[key], "data_versions.{key} differs");
    }
    // integrity digests are equal once rebuilt under a shared stamp.
    let key_of = |repo: &SqliteCompactMarket| {
        repo.read(|c| {
            c.query_row(
                "SELECT version_key FROM data_versions WHERE version_id='v1'",
                [],
                |r| r.get(0),
            )
            .map_err(storage)
        })
        .unwrap()
    };
    let stamp = "2025-01-02T00:00:00+00:00";
    let ia = repo1
        .read(|c| {
            compact_versions::recompute_integrity(c, key_of(&repo1), stamp).map(|d| hex_bytes(&d))
        })
        .unwrap();
    let ib = repo2
        .read(|c| {
            compact_versions::recompute_integrity(c, key_of(&repo2), stamp).map(|d| hex_bytes(&d))
        })
        .unwrap();
    assert_eq!(ia, ib, "integrity digests differ under shared stamp");
}

#[test]
fn collection_batch_records_item_failure_and_keeps_others() {
    let dir = tempdir();
    let (_, repo) = repo(&dir);
    // Task with two items: one valid, one with unconfirmed units.
    let scope = task_control::legacy::scope_id("sh.600006", "day");
    let scope2 = task_control::legacy::scope_id("sz.000001", "day");
    let work = FrozenWork {
        items: vec![
            ItemSpec {
                item_id: "item-a".into(),
                target: json!({"symbol": "sh.600006", "period": "day"}),
                progress_unit: "bars".into(),
                frozen_inputs: json!({}),
                scope_keys: BTreeSet::from([scope.clone()]),
            },
            ItemSpec {
                item_id: "item-b".into(),
                target: json!({"symbol": "sz.000001", "period": "day"}),
                progress_unit: "bars".into(),
                frozen_inputs: json!({}),
                scope_keys: BTreeSet::from([scope2.clone()]),
            },
        ],
        scopes: BTreeSet::from([scope, scope2]),
        shared_evidence: None,
    };
    let command = Command {
        key: CommandKey {
            operation: "collect_data".into(),
            dataset: "market.daily".into(),
            contract_version: "v1".into(),
        },
        intent: json!({}),
    };
    let snapshot = match repo
        .accept("req-batch", &command, &work, RUN, None)
        .unwrap()
        .record
        .outcome
    {
        task_control::RequestOutcome::Task(s) => s,
        _ => panic!("expected task"),
    };
    let task_id = snapshot.task.task_id.clone();
    // Claim the task (run_items does this in production).
    let head = repo.task_head(&task_id).unwrap();
    use crate::task_control::TaskRepository;
    repo.transition(&task_id, head.state_revision, RUN, Change::Claim)
        .unwrap();

    let mut good = collection_candidate_for(
        &mk_version("v-a", vec![bar(0, 100), bar(1, 110)]),
        &task_id,
        RUN,
    );
    good.item_id = "item-a".into();
    good.publication_id = "pub-a".into();
    let mut bad = collection_candidate_for(&mk_version("v-b", vec![bar(0, 100)]), &task_id, RUN);
    bad.item_id = "item-b".into();
    bad.symbol = "sz.000001".into();
    bad.publication_id = "pub-b".into();
    bad.unit_currency.price_unit = EvidenceValue::unknown("unconfirmed");

    let results = repo
        .commit_collection_batch(&task_id, &[good, bad])
        .unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].as_ref().unwrap().version_id, "v-a");
    assert!(results[1].is_err());
    let task = repo.task(&task_id).unwrap();
    assert_eq!(task.task.status, crate::task_control::Status::Running);
    assert_eq!(
        task.items
            .iter()
            .filter(|i| i.outcome.as_ref().is_some_and(|o| o.success))
            .count(),
        1
    );
    assert!(
        task.items
            .iter()
            .all(|i| i.stage == crate::task_control::Stage::Finished)
    );
    // The failed item produced no rows: only one published version exists.
    let heads = repo
        .read(|c| {
            c.query_row("SELECT count(*) FROM effective_heads", [], |r| {
                r.get::<_, i64>(0)
            })
            .map_err(storage)
        })
        .unwrap();
    assert_eq!(heads, 1);
}
